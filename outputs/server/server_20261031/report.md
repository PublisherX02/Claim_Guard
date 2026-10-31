# 66. Server experiment results: run `server_20261031`

Produced by `experiments/server/` (plan, targets and method: [docs/65](65_Server_Experiment_Plan.md)). Every number below was computed from the files in `outputs/server/server_20261031/`.

## 1. The machine

| Item | Value |
|---|---|
| Host | not available |
| Operating system | not available |
| CPU | None (None threads) |
| Memory | not available |
| GPU | not available |
| Docker | not available |
| Compose | not available |
| Code version | not available |
| Run started | not available |

## 2. Scoreboard

| Experiment | Measure | Target | Result | Verdict |
|---|---|---|---|---|
| S6 | internal ports closed | yes | no | FAIL |
| S6 | https open | yes | yes | PASS |
| S6 | http redirects to https | yes | no | FAIL |
| S6 | old tls refused | yes | yes | PASS |
| S6 | security headers present | yes | yes | PASS |
| S6 | anonymous api refused | yes | yes | PASS |
| S6 | anonymous health panel refused | yes | yes | PASS |
| S6 | healthz is minimal | yes | yes | PASS |

6 of 8 measures pass; 2 fail.

## 3. S6. Exposure seen from the network

| Port | Service | Open |
|---|---|---|
| 22 | SSH | no |
| 80 | HTTP (redirect only) | yes |
| 443 | HTTPS | yes |
| 3000 | app | no |
| 5432 | database | no |
| 6379 | Redis | no |
| 8000 | model server | no |
| 8080 | API | yes |
| 8091 | embedding service | no |
| 8095 | document reader | no |
| 11434 | model server (Ollama) | no |
| 27017 | MongoDB | no |

TLS: older protocols {"TLS1.0": "refused", "TLS1.1": "refused"}, negotiated TLSv1.3, certificate kernelsi.com issued by US Google Trust Services WE1, 84 days left. Headers: {"strict-transport-security": true, "x-content-type-options": true, "content-security-policy": true, "framing_protected": true, "no_server_version": true}.

## 4. How to read this

A FAIL is a finding, not an error in the scripts: write what was changed (a setting, a model, a limit) and run the failing experiment again, keeping both runs. Targets and their reasons are in docs/65 section 2. Everything here used synthetic claims; real hospital paper, handwriting and Arabic documents are still untested.
