<#--
  Shiki ログインテーマの共通レイアウト（base/login/template.ftl を置き換える）。
  - 画面の骨格（ブランド・カード・メッセージ・フッター）は独自。各画面は <#nested> で中身を差し込む。
  - Keycloak が前提にしているスクリプト（認証セッション監視・言語メニュー・WebAuthn の
    rfc4648 importmap）は base と同じものを読み込む。消すとパスキーや多タブ検知が壊れる。
  - showTryAnotherWay=false の画面（login.ftl）は「別の方法を試す」を自前のボタンで出す。
-->
<#macro registrationLayout bodyClass="" displayInfo=false displayMessage=true displayRequiredFields=false showTryAnotherWay=true>
<!DOCTYPE html>
<html class="${properties.kcHtmlClass!}" lang="${lang}"<#if realm.internationalizationEnabled> dir="${(locale.rtl)?then('rtl','ltr')}"</#if>>
<head>
    <meta charset="utf-8">
    <meta http-equiv="Content-Type" content="text/html; charset=UTF-8" />
    <meta name="robots" content="noindex, nofollow">
    <meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">
    <meta name="color-scheme" content="light dark">
    <title>${msg("loginTitle",(realm.displayName!''))}</title>
    <link rel="icon" href="${url.resourcesPath}/img/favicon.svg" type="image/svg+xml" />
    <#if properties.styles?has_content>
        <#list properties.styles?split(' ') as style>
            <link href="${url.resourcesPath}/${style}" rel="stylesheet" />
        </#list>
    </#if>
    <script type="importmap">
        {
            "imports": {
                "rfc4648": "${url.resourcesCommonPath}/vendor/rfc4648/rfc4648.js"
            }
        }
    </script>
    <script src="${url.resourcesPath}/js/menu-button-links.js" type="module"></script>
    <#if scripts??>
        <#list scripts as script>
            <script src="${script}" type="text/javascript"></script>
        </#list>
    </#if>
    <script type="module">
        import { startSessionPolling } from "${url.resourcesPath}/js/authChecker.js";

        startSessionPolling(
            "${url.ssoLoginInOtherTabsUrl?no_esc}"
        );
    </script>
    <script type="module">
        document.addEventListener("click", (event) => {
            const link = event.target.closest("a[data-once-link]");
            if (!link) {
                return;
            }
            if (link.getAttribute("aria-disabled") === "true") {
                event.preventDefault();
                return;
            }
            link.setAttribute("role", "link");
            link.setAttribute("aria-disabled", "true");
        });
    </script>
    <#if authenticationSession??>
        <script type="module">
            import { checkAuthSession } from "${url.resourcesPath}/js/authChecker.js";

            checkAuthSession(
                "${authenticationSession.authSessionIdHash}"
            );
        </script>
    </#if>
</head>

<#assign month = .now?string("M")?number>
<#assign season = (month gte 3 && month lte 5)?then("spring", (month gte 6 && month lte 8)?then("summer", (month gte 9 && month lte 11)?then("autumn", "winter")))>
<body class="${properties.kcBodyClass!} ${bodyClass}" data-page-id="login-${pageId}" data-season="${season}">
<div class="${properties.kcLoginClass!}">
    <main class="sk-main">
        <div class="sk-brand" id="kc-header">
            <span class="sk-logo" id="kc-header-wrapper">
                <#-- 四つの点（左上から時計回りに春・夏・秋・冬）。web/src/components/shell/brand-logo.tsx と同じ形。 -->
                <svg class="sk-logo-mark" viewBox="0 0 64 64" aria-hidden="true">
                    <circle cx="21" cy="21" r="8.6" fill="var(--season-spring)"/>
                    <circle cx="43" cy="21" r="8.6" fill="var(--season-summer)"/>
                    <circle cx="43" cy="43" r="8.6" fill="var(--season-autumn)"/>
                    <circle cx="21" cy="43" r="8.6" fill="var(--season-winter)"/>
                </svg>
                <span class="sk-wordmark">Shiki</span>
            </span>
        </div>

        <div class="${properties.kcFormCardClass!}">
            <header class="${properties.kcFormHeaderClass!}">
                <#if realm.internationalizationEnabled && locale.supported?size gt 1>
                    <div class="${properties.kcLocaleMainClass!}" id="kc-locale">
                        <div id="kc-locale-wrapper" class="${properties.kcLocaleWrapperClass!}">
                            <div id="kc-locale-dropdown" class="menu-button-links ${properties.kcLocaleDropDownClass!}">
                                <button tabindex="1" id="kc-current-locale-link" aria-label="${msg("languages")}" aria-haspopup="true" aria-expanded="false" aria-controls="language-switch1">
                                    <i class="sk-i sk-i-globe" aria-hidden="true"></i>
                                    <span>${locale.current}</span>
                                </button>
                                <ul role="menu" tabindex="-1" aria-labelledby="kc-current-locale-link" aria-activedescendant="" id="language-switch1" class="${properties.kcLocaleListClass!}">
                                    <#assign i = 1>
                                    <#list locale.supported as l>
                                        <li class="${properties.kcLocaleListItemClass!}" role="none">
                                            <a role="menuitem" id="language-${i}" class="${properties.kcLocaleItemClass!}" href="${l.url}">${l.label}</a>
                                        </li>
                                        <#assign i++>
                                    </#list>
                                </ul>
                            </div>
                        </div>
                    </div>
                </#if>

                <h1 id="kc-page-title" class="sk-title"><#nested "header"></h1>

                <#if auth?has_content && auth.showUsername() && !auth.showResetCredentials()>
                    <#nested "show-username">
                    <div id="kc-username" class="sk-identity">
                        <#assign attempted = auth.attemptedUsername!''>
                        <span class="sk-identity-avatar" aria-hidden="true"><#if attempted?has_content>${attempted[0]?upper_case}</#if></span>
                        <span id="kc-attempted-username" class="sk-identity-name">${auth.attemptedUsername}</span>
                        <a id="reset-login" class="sk-identity-reset" href="${url.loginRestartFlowUrl}" aria-label="${msg("restartLoginTooltip")}">
                            ${msg("shikiSwitchAccount")}
                        </a>
                    </div>
                </#if>

                <#if displayRequiredFields>
                    <p class="sk-required-note"><span class="sk-required">*</span> ${msg("requiredFields")}</p>
                </#if>
            </header>

            <div id="kc-content" class="sk-content">
                <div id="kc-content-wrapper">
                    <#-- アプリ起点のアクション（パスキー追加など）では「必須アクション」の警告は出さない。 -->
                    <#if displayMessage && message?has_content && (message.type != 'warning' || !isAppInitiatedAction??)>
                        <div class="${properties.kcAlertClass!} sk-alert-${message.type}" role="<#if message.type = 'error'>alert<#else>status</#if>">
                            <#if message.type = 'success'><span class="${properties.kcFeedbackSuccessIcon!}" aria-hidden="true"></span></#if>
                            <#if message.type = 'warning'><span class="${properties.kcFeedbackWarningIcon!}" aria-hidden="true"></span></#if>
                            <#if message.type = 'error'><span class="${properties.kcFeedbackErrorIcon!}" aria-hidden="true"></span></#if>
                            <#if message.type = 'info'><span class="${properties.kcFeedbackInfoIcon!}" aria-hidden="true"></span></#if>
                            <span class="${properties.kcAlertTitleClass!}">${kcSanitize(message.summary)?no_esc}</span>
                        </div>
                    </#if>

                    <#nested "form">

                    <#if showTryAnotherWay && auth?has_content && auth.showTryAnotherWayLink()>
                        <form id="kc-select-try-another-way-form" class="sk-try-another" action="${url.loginAction}" method="post">
                            <input type="hidden" name="tryAnotherWay" value="on"/>
                            <button type="submit" id="try-another-way" class="sk-link-button">${msg("doTryAnotherWay")}</button>
                        </form>
                    </#if>

                    <#nested "socialProviders">

                    <#if displayInfo>
                        <div id="kc-info" class="${properties.kcSignUpClass!}">
                            <div id="kc-info-wrapper" class="${properties.kcInfoAreaWrapperClass!}">
                                <#nested "info">
                            </div>
                        </div>
                    </#if>
                </div>
            </div>
        </div>
    </main>
</div>
</body>
</html>
</#macro>
