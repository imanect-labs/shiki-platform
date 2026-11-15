package jp.imanect.shiki.keycloak;

import org.keycloak.component.ComponentModel;
import org.keycloak.component.ComponentValidationException;
import org.keycloak.models.KeycloakSession;
import org.keycloak.models.RealmModel;
import org.keycloak.storage.ldap.LDAPStorageProvider;
import org.keycloak.storage.ldap.mappers.AbstractLDAPStorageMapper;
import org.keycloak.storage.ldap.mappers.membership.group.GroupLDAPStorageMapperFactory;

/** Keycloak 26.2.5専用。ADのGUIDを不変な内部グループ名にする。 */
public final class GuidGroupMapperFactory extends GroupLDAPStorageMapperFactory {
    public static final String ID = "shiki-ad-guid-group-mapper";

    @Override public String getId() { return ID; }
    @Override public String getHelpText() {
        return "Shiki AD groups: immutable objectGUID identity, flat groups, read-only membership.";
    }
    @Override public void validateConfiguration(KeycloakSession session, RealmModel realm, ComponentModel model) {
        requireConfig(model);
        super.validateConfiguration(session, realm, model);
    }
    static void requireConfig(ComponentModel model) {
        java.util.Map<String,String> fixed = java.util.Map.of(
            "mode", "READ_ONLY", "group.name.ldap.attribute", "objectGUID",
            "preserve.group.inheritance", "false",
            "user.roles.retrieve.strategy", "LOAD_GROUPS_BY_MEMBER_ATTRIBUTE_RECURSIVELY",
            "membership.ldap.attribute", "member", "membership.attribute.type", "DN",
            "member.of.ldap.attribute", "memberOf", "mapped.group.attributes", "cn,distinguishedName,memberOf");
        for (var entry : fixed.entrySet()) {
            if (!entry.getValue().equals(model.getConfig().getFirst(entry.getKey()))) {
                throw new ComponentValidationException("Invalid Shiki GUID mapper setting: " + entry.getKey());
            }
        }
    }
    @Override protected AbstractLDAPStorageMapper createMapper(ComponentModel model, LDAPStorageProvider provider) {
        return new GuidGroupMapper(model, provider, this);
    }
}
