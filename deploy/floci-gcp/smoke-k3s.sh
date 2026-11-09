#!/usr/bin/env bash
# Run only against the sidecar created by this experiment.
set -euo pipefail
sidecar=floci-gcp-shiki-floci-gke-shiki-local-shiki-local
docker_local=(env -u DOCKER_HOST -u DOCKER_CONTEXT -u DOCKER_TLS
  -u DOCKER_TLS_VERIFY -u DOCKER_CERT_PATH docker --host=unix:///var/run/docker.sock)
kube=("${docker_local[@]}" exec "$sidecar" kubectl
  --kubeconfig=/etc/rancher/k3s/k3s.yaml)
"${kube[@]}" wait --for=condition=Ready nodes --all --timeout=60s
job="shiki-infra-smoke-$(date +%s)"
trap '"${kube[@]}" delete job "$job" --ignore-not-found --wait=false' EXIT
"${kube[@]}" create job "$job" --image=busybox:1.37 -- sh -c 'echo shiki-kubernetes-smoke-ok'
"${kube[@]}" wait --for=condition=complete "job/$job" --timeout=60s
"${kube[@]}" logs "job/$job" | rg -x shiki-kubernetes-smoke-ok
echo 'PASS: actual Kubernetes node Ready and Job completed'
