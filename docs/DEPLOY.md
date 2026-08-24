# Deploying Sivana (PLAN §88, Phase 10)

## Topology

```text
                    object storage / shared volume
                    ┌──────────────────────────────┐
   sivana-ingest ──▶│  catalog-0000NN.siv (segments)│
   (writer node)    │  manifest.json  (swap point) │
                    └──────────────┬───────────────┘
                                   │ watched every 5 s
              ┌────────────────────┼────────────────────┐
              ▼                    ▼                    ▼
        matcher node          matcher node          matcher node
     sivana-api --catalog  sivana-api --catalog  sivana-api --catalog
     (mmap index + sidecar fingerprints, read-only)
              ▲                    ▲                    ▲
              └────────── load balancer (health-aware) ──┘
                                   │
                        browsers / extension (SFP1 over WS)
```

Matcher nodes are **stateless readers**: binary + memory-mapped segments +
sidecar fingerprints. Nothing in a query path mutates shared state.

## Running one node

```bash
cargo run -p sivana-ingest --release -- add --catalog /srv/catalog /path/to/audio
cargo run -p sivana-api  --release -- --catalog /srv/catalog --web apps/web \
  # listens on $SIVANA_ADDR, default 127.0.0.1:8077
```

- `GET /v1/health` returns `catalog_version` — route health checks on it;
  nodes reporting a stale version are draining, not broken.
- The watcher polls `manifest.json` every 5 s; on change it rebuilds the
  in-memory bundle and swaps atomically. Verified live: v2 → v3 under
  traffic with zero dropped sessions.
- Rollback = re-write the previous `manifest.json`; nodes converge on the
  next poll.

## Catalog lifecycle

- Ingestion is idempotent per SHA-256 source hash; re-run freely.
- Every ingest writes an immutable delta segment + atomic manifest swap.
- `sivana-ingest compact` merges all active segments into one and prunes;
  run it when segment count grows (nodes reload transparently).

## Measured performance on the reference dev box

(12th-gen i7 laptop, Windows, release build; absolute numbers include
~70% background load during capture — treat as ceilings.)

| metric | value | source |
|---|---|---|
| .siv lookup (512 hashes, 200k postings) | ~12 µs total (~23 ns/hash) | criterion `index_lookup` |
| match latency (Engine A, small catalog) | ~0.3 ms/query | bench grid |
| no-match session wall time | ~12 s server evidence window + overhead | loadgen, 24/24 ok |
| fingerprint realtime factor (native) | >50x floor enforced in tests | sivana-wasm test |
| catalog hot swap | ≤ 5 s propagation, zero downtime | this document |

## Scaling notes

- **Read scaling**: add matcher nodes; nothing to coordinate except the
  shared catalog directory. Segments are immutable so caching is safe.
- **Geo/CDN**: ship `manifest.json` + segments through any object store;
  nodes can poll an HTTPS mirror of the manifest instead of a volume.
- **Write scaling**: only ingestion touches segments; run one writer
  (or shard by source hash) and compact periodically.
- **Abuse posture** (§39): batch size cap (256 KiB), session TTL,
  bounded trailing window per session; rate-limit at the LB.

## Known limits

- Matcher index rebuilds from sidecars on swap: O(catalog) work per
  swap, currently seconds for small catalogs. Large deployments should
  mmap .siv directly in the query path (the format is ready; the
  InMemoryIndex bridge is the stopgap).
- Single writer per catalog directory (no distributed ingest locking).
