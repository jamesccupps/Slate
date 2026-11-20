# Ideas

New ideas go here first. Once one is planned it moves to [ROADMAP.md](ROADMAP.md), which has the order and the
sizes of everything that's coming.

## Not planned yet
- PPCL: go to the line a `GOTO` or `GOSUB` names (F12 or Ctrl+click), and mark a `GOTO` whose line isn't there (S).
- Word wrap in minified JSON: rows break inside a key or a number (`"balance":11` / `867.04`): DirectWrite finds few
  places to break there (spaces, hyphens, before a `{`) and cuts anywhere when a run is longer than a row. Breaking
  after `,` and `:` as VS Code does needs Slate's own line breaking for those rows (M; from the 2026-10-13 GUI
  review).

## Done

What each release brought is in [ROADMAP.md](ROADMAP.md) (and in each version's release notes); this list stopped at
0.7.0.
