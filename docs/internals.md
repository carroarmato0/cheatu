# Internals

## How the engine works

- **First scan** walks the target's readable+writable regions from
  `/proc/<pid>/maps`, reading them in 1 MiB chunks via `/proc/<pid>/mem` and
  testing each type-aligned slot against your value.
- **Next scan** re-reads only the surviving addresses and applies the chosen
  comparison against either your value or the previously observed value
  (for `increased`/`decreased`/`changed`/`unchanged`).
- Floating-point matches use a small tolerance so a displayed `100` still
  matches an in-memory `99.99998`.

## Scanning performance

- **Multithreaded:** a first scan splits memory into fixed-size work items and
  distributes them across a pool of worker threads (up to 16, sized to your
  CPU) that share one `/proc/<pid>/mem` handle via thread-safe `pread`. A ~1 GiB
  process scans in well under a second on a typical multi-core machine.
- **Pause target while scanning** (checkbox, on by default): cheatu sends the
  process `SIGSTOP` for the duration of the scan and `SIGCONT` right after, so
  values can't move mid-scan and there's no CPU contention. Turn it off if you
  need the game to keep running during the scan.

## Workspace layout

| Crate | Purpose |
|-------|---------|
| `cheatu-core` | The engine: process listing, `/proc/<pid>/maps` parsing, memory read/write, typed scan values, the first/next scan algorithm, and privilege helpers. UI-agnostic. |
| `cheatu-cli`  | The `cheatu` command-line REPL. |
| `cheatu-gui`  | The `cheatu-gui` graphical app. |
| `cheatu-inject` | The RPG Maker MV/MZ (NW.js) JavaScript-injection backend. |

## Testing

```sh
cargo test        # or: make test
```

The engine tests scan and patch the test process's *own* memory (always
accessible via `/proc/self/mem`), so they validate the full read → scan →
narrow → write pipeline without needing root.
