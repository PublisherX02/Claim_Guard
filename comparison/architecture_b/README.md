# Architecture B: the comparison arm (not part of ClaimGuard's shipped path)

This folder is a tool-using language-model agent adapted from the team's earlier clinicProj, kept so that `docs/24` can
compare it with ClaimGuard's rules-first design (Architecture A) on the same claims. **It is not deployed, nothing in
`src/` imports it, and it is deliberately not hardened beyond what the comparison harness needs.** Do not copy its
patterns into the product.

Known weaknesses, all recorded in `docs/24` and `outputs/architecture_comparison/`:

- **Prompt injection.** `validate_claim` places the whole claim, including the free-text `notes`, in the prompt, and the
  reply is returned with no output schema and no citation grounding. In the one recorded probe, a claim with a genuine
  currency violation (R015) and an injected instruction in its notes came back as `overall_status: VALID` with no
  findings. The `is_unsafe_input` filter checks violence and self-harm keywords only; it does nothing about injection.
- **Calculator tool.** It evaluates an expression the model chose. A character whitelist blocks names and calls, so code
  execution is not possible, and `calc_guard.py` (added after the recorded runs) now also rejects exponentiation and
  expressions over 200 characters, because exponent chains such as `9**9**9` did not finish (a test run was stopped after five seconds). The tool still calls
  the built-in evaluator behind that guard, so the recorded security scan (two dangerous sinks) still describes this code.
- **Interactive prompt.** The `__main__` block reads from `input()`.

Why it matters: these are the properties the comparison measures. Hardening them here would make the comparison unfair
and hide the finding. They are the reason the product keeps the model out of the verdict: it explains findings and
cannot create, remove or change one.
