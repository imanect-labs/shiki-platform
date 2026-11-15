#!/usr/bin/env python3
"""実Keycloak上のSPI登録・設定検証。ADへ接続せず、一時データは最後に撤去する。"""
import json
import os
import urllib.error
import urllib.parse
import urllib.request
import uuid

base = os.environ.get("KC", "http://localhost:8081").rstrip("/")
realm = os.environ.get("REALM", "shiki")
opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
token = None


def request(method, path, *, body=None, form=None, expected=(200,)):
    headers = {}
    data = None
    if token:
        headers["Authorization"] = f"Bearer {token}"
    if body is not None:
        data = json.dumps(body).encode()
        headers["Content-Type"] = "application/json"
    if form is not None:
        data = urllib.parse.urlencode(form).encode()
        headers["Content-Type"] = "application/x-www-form-urlencoded"
    req = urllib.request.Request(base + path, data=data, headers=headers, method=method)
    try:
        response = opener.open(req, timeout=30)
    except urllib.error.HTTPError as error:
        response = error
    with response:
        if response.status not in expected:
            raise RuntimeError(f"{method} {path}: unexpected status {response.status}")
        payload = response.read()
        return response.status, json.loads(payload) if payload else None, response.headers


_, auth, _ = request("POST", f"/realms/{urllib.parse.quote(realm, safe='')}/protocol/openid-connect/token", form={
    "grant_type": "client_credentials",
    "client_id": os.environ.get("PROVISIONER_CLIENT_ID", "shiki-provisioner"),
    "client_secret": os.environ.get("PROVISIONER_CLIENT_SECRET", "shiki-provisioner-dev-secret"),
})
token = auth["access_token"]
api = f"/admin/realms/{urllib.parse.quote(realm, safe='')}"
_, realm_info, _ = request("GET", api)
name = "guid-smoke-" + uuid.uuid4().hex
provider = None
root = None
try:
    _, _, headers = request("POST", api + "/groups", body={"name": name}, expected=(201,))
    root = headers["Location"].rsplit("/", 1)[-1]
    _, _, headers = request("POST", api + "/components", expected=(201,), body={
        "name": name, "parentId": realm_info["id"],
        "providerType": "org.keycloak.storage.UserStorageProvider", "providerId": "ldap",
        "config": {
            "enabled": ["false"], "vendor": ["ad"], "editMode": ["READ_ONLY"],
            "importEnabled": ["true"], "syncRegistrations": ["false"],
            "connectionUrl": ["ldaps://directory.invalid:636"],
            "usersDn": ["OU=Users,DC=example"], "authType": ["simple"],
            "bindDn": ["CN=reader,DC=example"], "bindCredential": ["unusable-test-value"],
            "usernameLDAPAttribute": ["sAMAccountName"], "rdnLDAPAttribute": ["cn"],
            "uuidLDAPAttribute": ["objectGUID"], "userObjectClasses": ["person,organizationalPerson,user"],
        },
    })
    provider = headers["Location"].rsplit("/", 1)[-1]
    config = {
        "groups.dn": ["OU=Groups,DC=example"], "groups.path": ["/" + name],
        "group.name.ldap.attribute": ["objectGUID"], "group.object.classes": ["group"],
        "membership.ldap.attribute": ["member"], "membership.attribute.type": ["DN"],
        "member.of.ldap.attribute": ["memberOf"], "mode": ["READ_ONLY"],
        "preserve.group.inheritance": ["false"],
        "mapped.group.attributes": ["cn,distinguishedName,memberOf"],
        "user.roles.retrieve.strategy": ["LOAD_GROUPS_BY_MEMBER_ATTRIBUTE_RECURSIVELY"],
    }
    mapper = {"name": "guid-groups", "parentId": provider,
              "providerType": "org.keycloak.storage.ldap.mappers.LDAPStorageMapper",
              "providerId": "shiki-ad-guid-group-mapper", "config": config}
    _, _, headers = request("POST", api + "/components", body=mapper, expected=(201,))
    identity = headers["Location"].rsplit("/", 1)[-1]
    _, saved, _ = request("GET", api + "/components/" + identity)
    assert saved["providerId"] == "shiki-ad-guid-group-mapper"
    request("GET", api + "/groups/" + root + "/members")
    for key, invalid in [("mode", "LDAP_ONLY"),
                         ("user.roles.retrieve.strategy", "GET_GROUPS_FROM_USER_MEMBEROF_ATTRIBUTE")]:
        broken = {**mapper, "name": "invalid-" + key.replace(".", "-"),
                  "config": {**config, key: [invalid]}}
        request("POST", api + "/components", body=broken, expected=(400,))
    print("GUID mapper registration and READ_ONLY / strategy validation: PASS")
finally:
    if provider:
        request("DELETE", api + "/components/" + provider, expected=(204, 404))
    if root:
        request("DELETE", api + "/groups/" + root, expected=(204, 404))
