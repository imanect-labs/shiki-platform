package jp.imanect.shiki.keycloak;

import java.util.Base64;
import org.junit.jupiter.api.Test;
import org.keycloak.component.ComponentModel;
import org.keycloak.component.ComponentValidationException;
import org.keycloak.models.GroupModel;
import org.keycloak.models.ModelException;
import org.keycloak.models.RealmModel;
import org.keycloak.models.UserModel;
import org.keycloak.storage.ldap.LDAPStorageProvider;
import org.keycloak.storage.UserStorageProviderModel;
import org.keycloak.storage.ldap.idm.model.LDAPObject;
import org.keycloak.storage.ldap.idm.store.ldap.LDAPUtil;
import static org.junit.jupiter.api.Assertions.*;
import static org.mockito.Mockito.*;

class GuidGroupMapperTest {
    static final String GUID = "00112233-4455-6677-8899-aabbccddeeff";
    static ComponentModel config() {
        ComponentModel model = new ComponentModel();
        model.getConfig().putSingle("mode", "READ_ONLY");
        model.getConfig().putSingle("group.name.ldap.attribute", "objectGUID");
        model.getConfig().putSingle("preserve.group.inheritance", "false");
        model.getConfig().putSingle("user.roles.retrieve.strategy", "LOAD_GROUPS_BY_MEMBER_ATTRIBUTE_RECURSIVELY");
        model.getConfig().putSingle("membership.ldap.attribute", "member");
        model.getConfig().putSingle("membership.attribute.type", "DN");
        model.getConfig().putSingle("member.of.ldap.attribute", "memberOf");
        model.getConfig().putSingle("mapped.group.attributes", "cn,distinguishedName,memberOf");
        return model;
    }
    static LDAPStorageProvider provider() {
        LDAPStorageProvider provider = mock(LDAPStorageProvider.class);
        ComponentModel parent = new ComponentModel();
        parent.getConfig().putSingle("uuidLDAPAttribute", "objectGUID");
        when(provider.getModel()).thenReturn(new UserStorageProviderModel(parent));
        return provider;
    }
    static LDAPObject group(String guid, String cn, String dn) {
        LDAPObject group = new LDAPObject();
        group.setUuid(guid);
        group.setSingleAttribute("cn", cn);
        group.setSingleAttribute("distinguishedName", dn);
        return group;
    }
    @Test void canonicalGuidUsesAdByteOrderAndNotTheRawBase64Name() {
        String base64 = "MyIRAFVEd2aImaq7zN3u/w==";
        LDAPObject group = group(LDAPUtil.decodeObjectGUID(Base64.getDecoder().decode(base64)), "Sales", "CN=Sales,OU=East");
        group.setSingleAttribute("objectGUID", base64);
        GuidGroupMapper.canonicalGroup(group);
        assertEquals(GUID, group.getAttributeAsString("objectGUID"));
        assertEquals("Sales", group.getAttributeAsString("cn"));
        assertNull(GuidGroupMapper.canonicalGroup(null));
        assertThrows(ModelException.class, () -> GuidGroupMapper.guid(base64));
        assertThrows(ModelException.class, () -> GuidGroupMapper.guid(null));
    }
    @Test void renameKeepsIdentityAndSameNameRecreationGetsANewIdentity() {
        LDAPObject original = group(GUID, "Sales", "CN=Sales,OU=East");
        LDAPObject renamed = group(GUID, "Renamed", "CN=Renamed,OU=West");
        LDAPObject recreated = group("11112233-4455-6677-8899-aabbccddeeff", "Sales", "CN=Sales,OU=East");
        assertEquals(GuidGroupMapper.canonicalGroup(original).getAttributeAsString("objectGUID"), GuidGroupMapper.canonicalGroup(renamed).getAttributeAsString("objectGUID"));
        assertNotEquals(original.getAttributeAsString("objectGUID"), GuidGroupMapper.canonicalGroup(recreated).getAttributeAsString("objectGUID"));
    }
    @Test void rejectsWritableAndNameBasedConfigurations() {
        GuidGroupMapperFactory.requireConfig(config());
        for (String key : new String[]{"mode", "group.name.ldap.attribute", "preserve.group.inheritance", "user.roles.retrieve.strategy", "membership.attribute.type"}) {
            ComponentModel invalid = config();
            invalid.getConfig().putSingle(key, "invalid");
            assertThrows(ComponentValidationException.class, () -> GuidGroupMapperFactory.requireConfig(invalid));
        }
    }
    @Test void managedMembershipAndLdapWritesAreRejectedBeforeAnyLookup() {
        GroupModel managed = mock(GroupModel.class);
        GuidGroupMapper mapper = new GuidGroupMapper(config(), provider(), new GuidGroupMapperFactory()) {
            @Override protected boolean isGroupInGroupPath(RealmModel realm, GroupModel group) { return group == managed; }
        };
        UserModel delegate = mock(UserModel.class);
        RealmModel realm = mock(RealmModel.class);
        assertNull(mapper.loadLDAPGroupByName("ad"));
        assertNull(mapper.loadLDAPGroupByName("legacy-sales"));
        UserModel user = mapper.proxy(new LDAPObject(), delegate, realm);
        assertThrows(ModelException.class, () -> user.joinGroup(managed));
        assertThrows(ModelException.class, () -> user.leaveGroup(managed));
        assertThrows(ModelException.class, () -> mapper.syncDataFromKeycloakToFederationProvider(realm));
        assertThrows(ModelException.class, () -> mapper.createLDAPGroup("x", null));
        assertThrows(ModelException.class, () -> mapper.updateLDAPGroup(null));
        assertThrows(ModelException.class, () -> mapper.addGroupMappingInLDAP(realm, managed, null));
        assertThrows(ModelException.class, () -> mapper.deleteGroupMappingInLDAP(null, null));
        verifyNoInteractions(delegate);
    }
    @Test void serviceRegistrationIsIncluded() {
        assertTrue(java.util.ServiceLoader.load(org.keycloak.storage.ldap.mappers.LDAPStorageMapperFactory.class)
            .stream().anyMatch(provider -> provider.type().equals(GuidGroupMapperFactory.class)));
    }
    @Test void unmanagedNativeGroupsStillUseTheirOwnDelegate() {
        GuidGroupMapper mapper = new GuidGroupMapper(config(), provider(), new GuidGroupMapperFactory()) {
            @Override protected boolean isGroupInGroupPath(RealmModel realm, GroupModel group) { return false; }
        };
        UserModel delegate = mock(UserModel.class);
        GroupModel nativeGroup = mock(GroupModel.class);
        UserModel user = mapper.proxy(new LDAPObject(), delegate, mock(RealmModel.class));
        user.joinGroup(nativeGroup);
        user.leaveGroup(nativeGroup);
        verify(delegate).joinGroup(nativeGroup);
        verify(delegate).leaveGroup(nativeGroup);
    }
}
