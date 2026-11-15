//! 停止後は新規取得せず、開始済みの反映を有限時間だけ待つ。
use crate::{keycloak_admin::KeycloakAdmin, AppState};
use authz::{AuthContext, Principal, PrincipalKind, TenantId};
use std::{future::Future, time::Duration};
use tokio::task::JoinSet;

pub async fn run(state: AppState, stop: impl Future<Output = ()>, grace: Duration) {
    if KeycloakAdmin::from_config(&state.http, &state.config.auth).is_err() {
        return;
    }
    tokio::pin!(stop);
    let store = state.directory.admin();
    let mut running = JoinSet::new();
    let mut interval = tokio::time::interval(Duration::from_secs(2));
    'poll: loop {
        tokio::select! {
            biased;
            () = &mut stop => break,
            Some(result) = running.join_next(), if !running.is_empty() => {
                if let Err(error) = result { tracing::error!(%error, "directory worker task failed"); }
                continue;
            }
            _ = interval.tick() => {}
        }
        if running.len() >= 4 {
            continue;
        }
        let pending = tokio::select! {
            biased;
            () = &mut stop => break,
            result = store.pending_operations() => result,
        };
        let pending = match pending {
            Ok(pending) => pending,
            Err(error) => {
                tracing::error!(%error,"directory operation queue unavailable");
                continue;
            }
        };
        for job in pending {
            tokio::select! {
                biased;
                () = &mut stop => break 'poll,
                () = std::future::ready(()) => {}
            }
            if running.len() >= 4 {
                break;
            }
            let Ok(tenant) = TenantId::parse(job.tenant_id) else {
                continue;
            };
            let ctx = AuthContext::new(
                Principal {
                    kind: PrincipalKind::User,
                    id: job.actor_id,
                    email: None,
                    groups: vec![],
                    roles: vec![],
                    tenant_id: Some(tenant.to_string()),
                },
                job.org,
                tenant,
            );
            let state = state.clone();
            running.spawn(async move {
                if let Err(error) = super::process(&state, &ctx, job.id).await {
                    tracing::error!(operation_id=%job.id,%error,"directory operation failed");
                }
            });
        }
    }
    drain(&mut running, grace).await;
}
async fn drain(running: &mut JoinSet<()>, grace: Duration) {
    if tokio::time::timeout(grace, async {
        while running.join_next().await.is_some() {}
    })
    .await
    .is_err()
    {
        tracing::warn!("directory shutdown grace exceeded; unfinished operations require recovery");
        running.abort_all();
        while running.join_next().await.is_some() {}
    }
}
#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    #[tokio::test]
    async fn shutdown_waits_for_started_work_and_aborts_after_grace() {
        let completed = Arc::new(AtomicBool::new(false));
        let flag = completed.clone();
        let mut jobs = JoinSet::new();
        jobs.spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            flag.store(true, Ordering::SeqCst);
        });
        drain(&mut jobs, Duration::from_secs(1)).await;
        assert!(completed.load(Ordering::SeqCst));
        assert!(jobs.is_empty());
        let flag = completed.clone();
        completed.store(false, Ordering::SeqCst);
        jobs.spawn(async move {
            tokio::time::sleep(Duration::from_secs(10)).await;
            flag.store(true, Ordering::SeqCst);
        });
        tokio::time::timeout(
            Duration::from_secs(1),
            drain(&mut jobs, Duration::from_millis(10)),
        )
        .await
        .unwrap();
        assert!(!completed.load(Ordering::SeqCst));
        assert!(jobs.is_empty());
    }
}
