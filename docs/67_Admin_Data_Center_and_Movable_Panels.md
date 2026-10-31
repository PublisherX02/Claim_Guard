# 67. The administrator's data center and the movable operator panels

This note records what was added after v1.0.0, how it works, what was tested and what was not.

## 1. The data-center board (administrators)

A new page, **Queue > Data center** (`#/board`), for anyone holding `queue.view` (level 4). It reads one endpoint once a minute and shows five tabs:

| Tab | What it shows |
|---|---|
| Overview | claims received today, processed today, still unprocessed (the whole backlog), rule violations, operators who worked, claims cleared by the system against decided by people; where the unprocessed claims sit by state; a 24-hour line |
| Live flow | claims received against processed, and rule violations, one point per hour for 24 hours; the same numbers as a table |
| Rules | how often each rule was violated today (and how often it could not be assessed) |
| Operators | each operator now (processing, waiting in their inbox, processed today, a load bar) and a productivity table (claims, decisions, confirmed, dismissed, information requested, first and last decision, claims per hour) |
| Reports | the **Download today's report (JSON)** button and a preview of what the file holds |

"Today" is the browser's own midnight, sent as `since`; the server clamps it to the last 48 hours.

### Definitions (also written into every report under `definitions`)

* **received**: a claim whose receipt was written inside the window.
* **processed**: a claim that reached `decided` or `rechecked` inside the window; split into decided by a person and cleared by the system.
* **unprocessed**: a claim not yet decided when the numbers are read, whenever it arrived (the backlog). Received through awaiting-countersign count.
* **violated**: a rule whose result on a received claim is FAIL. UNABLE_TO_ASSESS is counted on its own.
* an operator's **claims**: distinct claims on which that badge recorded a decision inside the window. The system's own automatic request-for-information is never credited to a person.
* decisions are counted when they go through the queue (the console's route, `POST /work/claims/{id}/findings/{rule}/decision`). A decision made through the direct reviewer route (`POST /claims/{id}/findings/{rule}/decision`, which bypasses the queue) is audited but is not in these numbers.
* **claims per hour**: claims decided divided by the hours between the operator's first and last decision, at least one hour.

### Endpoints (Rust server, `cg-api`; arithmetic in `cg-queue/src/stats.rs`)

* `GET /api/v1/queue/stats?since=<epoch seconds>`: the numbers above as JSON. Needs `queue.view`.
* `GET /api/v1/queue/report?since=<epoch seconds>`: the same numbers as a file download (`Content-Disposition: attachment; filename="claimguard-report-YYYY-MM-DD.json"`, `Cache-Control: no-store`) with `report.generated_by`, `report.generated_at` and `report.format = claimguard-daily-report/1`. **Every download is written to the audit trail** (`queue_admin`, command `daily_report_download`). Needs `queue.view`.

Both carry counts and badge numbers only: no claim content, no patient identifier (tested).

The Python reference server does not have these two routes; the shipped product is the Rust server.

## 2. Movable panels on the claim screen (operators)

Each part of the claim screen (clear or escalate, the flagged findings with the decision form, service lines, fix suggestions, patient history, claim facts, other rules, advisory checks) carries a grip. Dragging the grip with the mouse or a finger puts the panel above, below or in the other column. With the keyboard, focus the grip and use the arrow keys. **Reset layout** puts everything back.

* Panels are matched by a fixed key, so one arrangement applies to every claim.
* The arrangement is kept in this browser for this badge (`localStorage`, key `cg.panels.<badge>`). It does not follow the operator to another computer: the server stores no per-user preferences yet. This is a known limit.
* Nothing in a panel's content changes; the move only reorders existing elements.

## 3. What was tested

See section 4 of the pull request. The Rust arithmetic has unit tests in `stats.rs` (counting, windows, operators, system work not credited to people, hourly buckets, empty day) and an integration test in `cg-queue/tests/service_flow.rs` (permissions, audit of the download, no patient identifier). `rust/tools/pentest.py` now lists both routes as administrator-only. In the browser, against `cg serve --demo --dev --queue`, the Data center page was loaded as an administrator (all five tabs, live numbers from the 60 demo claims, the report download returned 200 with no patient identifier, no console errors) and the claim screen as an operator. The full Rust workspace suite (`cargo test --workspace --locked`) passes.

## 4. Not done

* No per-user layout on the server (see above).
* The Python server has no stats or report route.
* The draggable layout was driven in Chrome against the local demo with scripted pointer and keyboard events (drag across columns, arrow keys, saved after a reload, Reset layout). It was **not** tried with a real touch screen.

## 5. Re-run after the release (v1.1.0 build, 2026-10-31)

* `rust/tools/console_review.py` (a real Chrome, every demo role, every page it may see, desktop 1440 px and phone 390 px, claim opened in the work view; fails on console errors, HTTP errors, uncaught exceptions, sideways scroll) now also opens `#/board`: **0 issues**, 42 screenshots. The Data center was looked at in both widths (cards, bars and the line chart fit at 390 px; the tab strip scrolls sideways inside itself).
* `rust/tools/pentest.py` (black box, real sockets, own demo instance) with the two new routes listed as administrator-only: **125 checks, 0 failed**.
* CI on the merged main: build, Python 3.10/3.12/3.14, rules accuracy, Rust tests and `cargo audit`, security: all green.
* End to end on the demo: one operator confirmed the findings of 2 inbox claims through the queue route; the board then read processed 2 (by a person 2), unprocessed 60 to 58, that operator 2 claims / 2 confirmed, inbox 6 to 4, and the report download returned 200.
