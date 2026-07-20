# Postgres 17 with the EnterpriseDB pldebugger extension compiled from source.
# Stock postgres images do not ship pldebugger, so build it here.
FROM postgres:17

ARG PLDEBUGGER_REF=master

RUN set -eux; \
    apt-get update; \
    apt-get install -y --no-install-recommends \
        build-essential \
        git \
        ca-certificates \
        libkrb5-dev \
        postgresql-server-dev-17; \
    git clone https://github.com/EnterpriseDB/pldebugger.git /tmp/pldebugger; \
    cd /tmp/pldebugger; \
    git checkout "$PLDEBUGGER_REF"; \
    make USE_PGXS=1; \
    make USE_PGXS=1 install; \
    cd /; \
    rm -rf /tmp/pldebugger; \
    apt-get purge -y --auto-remove build-essential git libkrb5-dev postgresql-server-dev-17; \
    rm -rf /var/lib/apt/lists/*
