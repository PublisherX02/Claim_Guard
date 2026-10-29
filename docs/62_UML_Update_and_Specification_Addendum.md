# 62 | UML update and specification addendum (2026-10-29)

**Why.** The UML diagrams were last changed on 2026-10-06 and `SPECIFICATION.md` was checked on 2026-10-15. The Rust build then became the shipped system and gained features the model did not show. This note records what was changed, how, and what was not.

## What changed

| Area | Change |
|---|---|
| `SPECIFICATION.md` | New section 10 (addendum): 10.1 statements of sections 3 and 9.2 that no longer hold, each with the source file that proves it; 10.2 use cases UC30 to UC40 with route, permission and status; 10.3 the diagrams updated or added. Short pointers added in sections 3, 8 and 9.2. Where the addendum differs from sections 1 to 9, the addendum prevails. |
| Diagrams updated | 01 (UC30 to UC40), 02 (eleven advisory checks, `ImportDraft`), 03 (`Batch`, `InfoRequestEvent`, `Notification`, `Approval`, `PayerRulesVersion`, `LedgerCheckpoint`, `BatchStatus`, `rechecked`), 04 and 13 (worker task, Redis Streams, HTTP or import submission), 08 (new transitions), 12 (nine collections). |
| Diagrams relabelled | 09, 09b, 10, 11 are kept and titled as the Python reference. |
| Diagrams added | 09c (components, Rust), 10b (deployment, Rust), 11b (the twelve crates), 14 (file import), 15 (what-if, correct and recheck), 16 (batch with audit sample, quarantine and decision). There are now 20 diagrams in `docs/figures/uml/en/`. |
| `README.md` | The "not built" paragraph was out of date (it said no web screen, no HTTP submission, recheck 501, model not wired, no encryption at rest) and was rewritten; the diagram counts and the document list were updated. |
| `docs/33` and the French diagrams | Deleted at the owner's request: `docs/33_Cahier_des_Charges_et_Modelisation_UML.md`, the 13 French `.puml` files in `docs/uml/` and their images in `docs/figures/uml/`. Only the English set remains (`docs/uml/en/`, `docs/figures/uml/en/`, and the shared `docs/uml/style.iuml`). |

## How it was checked

Each statement in 10.1 and each route in 10.2 was read from the Rust sources (`cg-api`, `cg-queue`, `cg-engine`, `cg-mongo`, `deploy/rust/compose.yml`, each crate's `Cargo.toml`). The images were rendered with PlantUML 1.2026.8 and each changed one was looked at.

## Not done

- Diagrams 05, 06 and 07 were searched for statements that changed and left as they are. They were not re-verified step by step against the Rust code.
- `POST /claims/{id}/recheck` still returns 501 in the code; the diagrams show the working route.
- No new tests were run: the change is documentation and diagram sources only.
