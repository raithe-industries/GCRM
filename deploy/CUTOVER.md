# GCRM v2 — Production Cutover Runbook

**RAiTHE INDUSTRIES INCORPORATED © 2026.** Cutting the live `raithe.ca/risk` service over
to the v2 engine (theaters · systemic index · escalation ladder · structured-extraction
AI · rebuilt dashboard). Read once, top to bottom, before running anything.

The order matters: the running service appends to today's `logs/events_*.jsonl`, so the
**one-time backfill must run while the service is stopped**, between `stop` and `start`.

---

## 0. Pre-flight (no service impact)

```bash
cd ~/Desktop/GCRM
cargo test            # expect: 319 passed
cargo build --release # produces target/release/gcrm
```

Confirm Ollama is up and the chat model is pulled:

```bash
systemctl is-active ollama
ollama list | grep qwen2.5:7b          # pull if missing: ollama pull qwen2.5:7b
# only if you intend to enable llm.semantic_dedup:
# ollama pull nomic-embed-text
```

## 1. (Optional, recommended) Tune Ollama for the concurrent worker pool

Without this, Ollama serializes requests and the GCRM worker pool gains nothing.

```bash
sudo mkdir -p /etc/systemd/system/ollama.service.d
sudo cp deploy/ollama.service.d/override.conf \
        /etc/systemd/system/ollama.service.d/override.conf
sudo systemctl daemon-reload
sudo systemctl restart ollama
systemctl show ollama -p Environment    # verify OLLAMA_NUM_PARALLEL=8 etc.
```

Keep `OLLAMA_NUM_PARALLEL` equal to `llm.concurrency` in `settings.yml` (both 8).

## 2. Cut over GCRM — stop, backfill, start

```bash
sudo systemctl stop gcrm.service        # <-- stop FIRST (so backfill is race-free)

target/release/gcrm backfill            # tags archived events with their theater;
                                        # writes *.jsonl.bak; idempotent

sudo systemctl start gcrm.service       # loads the tagged window + v2 + AI@concurrency 8
```

> Do **not** use `systemctl restart` here — the backfill must run *between* stop and
> start. A restart would leave the old service running during the backfill.

## 3. Verify the live service

```bash
# health + headline
curl -s localhost:8000/risk/api/health
curl -s localhost:8000/risk/api/latest \
  | python3 -c 'import sys,json;d=json.load(sys.stdin);print("index",d["systemic"]["index"],"| P",d["probabilities"]["annual_pct"],"% |",d["systemic"]["driver"])'

# AI is actually running concurrently (not falling back to keyword-only):
journalctl -u gcrm -n 40 --no-pager | grep -E "LLM extraction|concurrency|brief"

# analyst brief is generating (source should become "llm" within ~5 min):
curl -s localhost:8000/risk/api/brief | python3 -c 'import sys,json;print(json.load(sys.stdin)["source"])'
```

Then open **https://raithe.ca/risk** — confirm the systemic index, theater ladder,
I&W board, and analyst-brief panel render, and `/risk/methodology` shows the v2 page.

## 4. Rollback (if needed)

```bash
sudo systemctl stop gcrm.service
# restore the pre-backfill archive:
cd ~/Desktop/GCRM/logs && for f in events_*.jsonl.bak; do mv "$f" "${f%.bak}"; done
# rebuild the previous binary from your last good commit, then:
sudo systemctl start gcrm.service
```

---

### Notes
- **AI on by default**: `llm.enabled: true`, `llm.concurrency: 8`. If Ollama is ever
  down, GCRM falls back silently to keyword scoring and a templated brief — it never
  blocks.
- **Transition behaviour without the backfill**: the service still runs, but archived
  events read as theater "other" and the systemic layer reads low until the live window
  refills (~hours). The backfill in step 2 avoids that.
- **`semantic_dedup`** stays off by default (a 2nd Ollama call per article). Enable in
  `settings.yml` only if the GPU has headroom; then `ollama pull nomic-embed-text`.
