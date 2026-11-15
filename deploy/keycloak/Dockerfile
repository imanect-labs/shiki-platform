ARG KEYCLOAK_VERSION=26.2.5
FROM maven:3.9.9-eclipse-temurin-21 AS guid-mapper
ARG KEYCLOAK_VERSION
WORKDIR /build
COPY guid-mapper/pom.xml ./pom.xml
COPY guid-mapper/src ./src
RUN --mount=type=cache,id=shiki-keycloak-maven,target=/root/.m2/repository \
    --mount=type=secret,id=maven_settings,target=/root/.m2/settings.xml \
    --mount=type=secret,id=network_ca,target=/tmp/network-ca.pem \
    if [ -f /tmp/network-ca.pem ]; then keytool -importcert -noprompt -alias build-network-ca -file /tmp/network-ca.pem -cacerts -storepass changeit; fi \
    && mvn -B -Dkeycloak.version=${KEYCLOAK_VERSION} verify

FROM quay.io/keycloak/keycloak:${KEYCLOAK_VERSION}
COPY --from=guid-mapper --chown=keycloak:keycloak /build/target/shiki-ad-guid-mapper.jar /opt/keycloak/providers/shiki-ad-guid-mapper.jar
RUN /opt/keycloak/bin/kc.sh build --db=postgres --health-enabled=true
