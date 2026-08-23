# Sivana Deployment (PLAN §88, Phase 10)

The recognition path scales as **stateless matcher nodes over an
immutable segment set**. Nothing in the query hot path is mutable.

## Topology

```text
                    object storage / shared disk
        catalog-000001.siv  catalog-000002.siv  manifest.json
                 │                    │            │
     ┌───────────┴────────────────────▼────────────┴───┐
     │ every matcher node watches manifest.json        │
     │  sivana-api --catalog /srv/sivana/catalog       │
     │   = binary + mmap'd .siv segments + sidecars    │
     └───────────┬────────────────────┬────────────────┘
                 │                    │
           load balancer (health-aware: /v1/health
           reports catalog_version; drain nodes that
           lag the newest version)
                 │
     browsers / extension  →  SFP1 fingerprints over WebSocket
```

## Matcher node

One process per machine:

```
sivana-api --catalog /srv/sivana/catalog [--web apps/web]
```

* Index memory is `mmap`; the OS page cache is the cache. A node needs
  RAM for metadata, not for the whole index.
* The node polls `manifest.json` every 5 s and atomically swaps the
  serving bundle when it changes — verified live (catalog v1 -> v2 under
  continuous traffic, zero dropped sessions).
* Sessions are per-connection with a 12 s eager timeout; nothing is
  shared between nodes, so nodes scale horizontally behind any LB.

## Catalog updates (ingest side)

```
sivana-ingest add     --catalog DIR files...   # parallel, idempotent (sha256 dedup)
sivana-ingest compact --catalog DIR            # merge delta segments into one
```

* `add` writes one immutable delta segment + fingerprint sidecars and
  swaps the manifest atomically (tmp + rename). Nodes pick it up within
  5 s.
* `compact` merges all active segments into a fresh single segment and
  prunes superseded files after the swap. Run it when the delta count
  grows; compaction is offline for readers by construction.
* Rollback = rewrite the previous manifest.json (see
  crates/sivana-index manifest tests).

## Distribution

Segments are plain files: rsync or push them to object storage (S3/GCS)
and have nodes fetch new ones before the manifest that references them.
Order matters — segments first, manifest last; the atomic rename is the
commit point.

## Capacity math (§54)

At ~26 bands/octave quantization the V2 engine emits roughly 60-90
fingerprints/second of audio (fixture-measured). At 8 bytes/posting a
1M-track x 4-min catalog lands around 40-80 GB of postings before
compression work — measure with `sivana-index` benches before capacity
commitments; density tuning is §54's economic lever.

## Load testing

```
cargo run -p sivana-bench --release -- loadgen \
  --url ws://matcher-1.internal:8077 --sessions 100 --concurrency 16
```

Reports session-latency p50/p95/p99. Baseline on the dev box: 24/24
sessions OK, p50 14.7 s / p95 14.7 s / p99 14.7 s for worst-case
out-of-catalog streams (the full 12 s evidence window dominates; the
flatness of the distribution — p50≈p99 — is the healthy signal).

## Observability (§58)

`/v1/health` exposes liveness + catalog_version. Per-query diagnostics
(inliers, concentration, capture time) already stream to clients and are
logged in the browser SYSTEM NOTES panel; shipping them to Prometheus is
the remaining §58 item and is deliberately deferred until there is a
deployment to observe.
