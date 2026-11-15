package jp.imanect.shiki.keycloak;

import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.UUID;
import org.keycloak.component.ComponentModel;
import org.keycloak.models.GroupModel;
import org.keycloak.models.ModelException;
import org.keycloak.models.RealmModel;
import org.keycloak.models.UserModel;
import org.keycloak.storage.ldap.LDAPStorageProvider;
import org.keycloak.storage.ldap.idm.model.LDAPObject;
import org.keycloak.storage.ldap.mappers.membership.group.GroupLDAPStorageMapper;
import org.keycloak.storage.user.SynchronizationResult;

/**
 * objectGUID属性は標準mapperでは生バイトのBase64になるが、UUID検索はUUID表示形式を要求する。
 * LDAPIdentityStoreが復号済みのgetUuid()を使用して、同期とログインの双方を同じ名前にする。
 * CN/SAM/DNの変更・再利用で既存の認可グループを別ADオブジェクトへ付け替えない。
 */
public class GuidGroupMapper extends GroupLDAPStorageMapper {
    public GuidGroupMapper(ComponentModel model, LDAPStorageProvider provider, GuidGroupMapperFactory factory) {
        super(model, provider, factory);
        GuidGroupMapperFactory.requireConfig(model);
        if (!"objectGUID".equals(provider.getModel().getConfig().getFirst("uuidLDAPAttribute"))) {
            throw new ModelException("Shiki AD mapper requires objectGUID as the LDAP UUID attribute");
        }
    }
    static String guid(String value) {
        try {
            if (value == null || value.length() != 36) throw new IllegalArgumentException();
            String canonical = UUID.fromString(value).toString();
            if (!canonical.equalsIgnoreCase(value)) throw new IllegalArgumentException();
            return canonical;
        } catch (IllegalArgumentException error) {
            throw new ModelException("Invalid AD objectGUID", error);
        }
    }
    static LDAPObject canonicalGroup(LDAPObject group) {
        if (group != null) group.setSingleAttribute("objectGUID", guid(group.getUuid()));
        return group;
    }
    @Override protected List<LDAPObject> getAllLDAPGroups(boolean includeMembers) {
        List<LDAPObject> groups = super.getAllLDAPGroups(includeMembers);
        groups.forEach(GuidGroupMapper::canonicalGroup);
        return groups;
    }
    @Override protected List<LDAPObject> getLDAPGroupMappings(LDAPObject user) {
        List<LDAPObject> groups = super.getLDAPGroupMappings(user);
        groups.forEach(GuidGroupMapper::canonicalGroup);
        return groups;
    }
    @Override public LDAPObject loadLDAPGroupByName(String name) {
        final String identity;
        try { identity = guid(name); }
        catch (ModelException legacyOrContainer) { return null; }
        return canonicalGroup(super.loadLDAPGroupByName(identity));
    }
    @Override public UserModel proxy(LDAPObject ldapUser, UserModel delegate, RealmModel realm) {
        return new LDAPGroupMappingsUserDelegate(realm, delegate, ldapUser) {
            @Override public boolean isMemberOf(GroupModel group) {
                return isGroupInGroupPath(realm, group) ? super.isMemberOf(group) : delegate.isMemberOf(group);
            }
            @Override public void joinGroup(GroupModel group) {
                if (isGroupInGroupPath(realm, group)) throw readOnly();
                delegate.joinGroup(group);
            }
            @Override public void leaveGroup(GroupModel group) {
                // 標準delegateは直接UUID名条件を組み立てる。LDAP検索前にREAD_ONLYを拒否する。
                if (isGroupInGroupPath(realm, group)) throw readOnly();
                delegate.leaveGroup(group);
            }
        };
    }
    private static ModelException readOnly() { return new ModelException("Shiki AD groups are read only"); }
    @Override public SynchronizationResult syncDataFromKeycloakToFederationProvider(RealmModel realm) { throw readOnly(); }
    @Override public LDAPObject createLDAPGroup(String name, Map<String, Set<String>> attributes) { throw readOnly(); }
    @Override public LDAPObject updateLDAPGroup(LDAPObject group) { throw readOnly(); }
    @Override public void addGroupMappingInLDAP(RealmModel realm, GroupModel group, LDAPObject user) { throw readOnly(); }
    @Override public void deleteGroupMappingInLDAP(LDAPObject user, LDAPObject group) { throw readOnly(); }
}
