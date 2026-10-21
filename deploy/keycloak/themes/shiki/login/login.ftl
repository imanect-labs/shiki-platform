<#--
  ログイン画面（ユーザー名＋パスワード、およびパスキーへの切替）。
  - browser フロー「shiki browser」は「パスワード」と「パスキー」を同じ段の ALTERNATIVE に置いている。
    パスキーの選択肢（authenticationSelections）があれば、select-authenticator 画面を経由せず
    直接その実行（authenticationExecution）へ切り替えるボタンを出す。
  - #username / #password / #kc-login の id は E2E（web/e2e/helpers.ts）とパスワードマネージャの
    互換のために base と同じものを保つ。
-->
<#import "template.ftl" as layout>
<#assign passkeyExecId = "">
<#if auth?has_content && auth.authenticationSelections??>
    <#list auth.authenticationSelections as selection>
        <#if selection.displayName == "webauthn-passwordless-display-name">
            <#assign passkeyExecId = selection.authExecId>
        </#if>
    </#list>
</#if>
<@layout.registrationLayout displayMessage=!messagesPerField.existsError('username','password') displayInfo=realm.password && realm.registrationAllowed && !registrationDisabled?? showTryAnotherWay=(passkeyExecId == ""); section>
    <#if section = "header">
        ${msg("loginAccountTitle")}
    <#elseif section = "form">
        <div id="kc-form">
            <#if passkeyExecId?has_content>
                <form id="kc-passkey-form" action="${url.loginAction}" method="post">
                    <input type="hidden" name="authenticationExecution" value="${passkeyExecId}"/>
                    <button type="submit" id="kc-passkey" class="sk-btn sk-btn-secondary sk-btn-block sk-btn-lg">
                        <i class="sk-i sk-i-key" aria-hidden="true"></i>
                        ${msg("shikiPasskeyLogin")}
                    </button>
                </form>
                <div class="sk-divider" role="separator"><span>${msg("shikiOr")}</span></div>
            </#if>

            <#if realm.password>
                <form id="kc-form-login" class="sk-form" onsubmit="login.disabled = true; return true;" action="${url.loginAction}" method="post" novalidate>
                    <#if !usernameHidden??>
                        <div class="sk-field">
                            <label for="username" class="sk-label"><#if !realm.loginWithEmailAllowed>${msg("username")}<#elseif !realm.registrationEmailAsUsername>${msg("usernameOrEmail")}<#else>${msg("email")}</#if></label>
                            <input tabindex="2" id="username" class="sk-input" name="username" value="${(login.username!'')}" type="text" autofocus autocomplete="username"
                                   aria-invalid="<#if messagesPerField.existsError('username','password')>true</#if>"
                                   <#if messagesPerField.existsError('username','password')>aria-describedby="input-error"</#if>
                                   dir="ltr" />
                        </div>
                    </#if>

                    <div class="sk-field">
                        <div class="sk-label-row">
                            <label for="password" class="sk-label">${msg("password")}</label>
                            <#if realm.resetPasswordAllowed>
                                <a tabindex="6" class="sk-link" href="${url.loginResetCredentialsUrl}">${msg("doForgotPassword")}</a>
                            </#if>
                        </div>
                        <div class="sk-input-group" dir="ltr">
                            <input tabindex="3" id="password" class="sk-input" name="password" type="password" autocomplete="current-password"
                                   aria-invalid="<#if messagesPerField.existsError('username','password')>true</#if>"
                                   <#if messagesPerField.existsError('username','password')>aria-describedby="input-error"</#if> />
                            <button class="sk-input-addon" type="button" aria-label="${msg("showPassword")}"
                                    aria-controls="password" data-password-toggle tabindex="4"
                                    data-icon-show="sk-i sk-i-eye" data-icon-hide="sk-i sk-i-eye-off"
                                    data-label-show="${msg('showPassword')}" data-label-hide="${msg('hidePassword')}">
                                <i class="sk-i sk-i-eye" aria-hidden="true"></i>
                            </button>
                        </div>
                        <#if messagesPerField.existsError('username','password')>
                            <span id="input-error" class="sk-field-error" aria-live="polite">
                                ${kcSanitize(messagesPerField.getFirstError('username','password'))?no_esc}
                            </span>
                        </#if>
                    </div>

                    <#if realm.rememberMe && !usernameHidden??>
                        <label class="sk-check">
                            <input tabindex="5" class="sk-check-input" id="rememberMe" name="rememberMe" type="checkbox" <#if login.rememberMe??>checked</#if>>
                            <span class="sk-check-label">${msg("rememberMe")}</span>
                        </label>
                    </#if>

                    <div id="kc-form-buttons" class="sk-buttons">
                        <input type="hidden" id="id-hidden-input" name="credentialId" <#if auth.selectedCredential?has_content>value="${auth.selectedCredential}"</#if>/>
                        <input tabindex="7" class="sk-btn sk-btn-primary sk-btn-block sk-btn-lg" name="login" id="kc-login" type="submit" value="${msg("doLogIn")}"/>
                    </div>
                </form>
            </#if>
        </div>
        <script type="module" src="${url.resourcesPath}/js/passwordVisibility.js"></script>
    <#elseif section = "info" >
        <#if realm.password && realm.registrationAllowed && !registrationDisabled??>
            <div id="kc-registration">
                <span>${msg("noAccount")} <a tabindex="8" class="sk-link" href="${url.registrationUrl}">${msg("doRegister")}</a></span>
            </div>
        </#if>
    <#elseif section = "socialProviders" >
        <#if realm.password && social?? && social.providers?has_content>
            <div id="kc-social-providers" class="sk-social">
                <div class="sk-divider" role="separator"><span>${msg("identity-provider-login-label")}</span></div>
                <ul class="sk-social-list">
                    <#list social.providers as p>
                        <li>
                            <a data-once-link id="social-${p.alias}" class="sk-btn sk-btn-secondary sk-btn-block" href="${p.loginUrl}">
                                <span class="sk-social-name">${p.displayName!}</span>
                            </a>
                        </li>
                    </#list>
                </ul>
            </div>
        </#if>
    </#if>
</@layout.registrationLayout>
