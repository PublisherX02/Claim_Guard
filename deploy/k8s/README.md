# ClaimGuard on Kubernetes (on premises)

`claimguard.yaml` is the same system as `deploy/rust/compose.yml`, expressed for a Kubernetes cluster in the hospital's own data centre (no cloud load balancer, no managed database). The Docker Compose stack remains the simpler path for a single server; use this when the hospital already runs Kubernetes.

## What you need
- A cluster with a CNI that enforces `NetworkPolicy` (Calico, Cilium), an ingress controller (ingress-nginx on a NodePort, or MetalLB in front of it), and a default `StorageClass`.
- The image in a registry the cluster can pull from: `docker build -f deploy/rust/Dockerfile -t registry.hospital.internal/claimguard-rs:1 .` then push, and put that name in the two `image:` lines (pin by digest).
- A TLS certificate for your host name as a Secret `claimguard-tls` (your internal CA, or cert-manager).

## Steps
```
kubectl apply -f deploy/k8s/claimguard.yaml            # creates the namespace first; re-run once if a resource says "namespace not found"
# secrets: same names as the Compose .env (deploy/env.production.example); install.sh generates the values
kubectl -n claimguard create secret generic claimguard-secrets --from-env-file=.env
kubectl -n claimguard create secret tls claimguard-tls --cert=tls.crt --key=tls.key
kubectl -n claimguard get pods -w
# the first administrator (level 4), from inside the cluster:
kubectl -n claimguard exec -it deploy/api -- cg admin create-admin --badge CG-4004 --name "First Admin"   # asks for the password; --password-stdin for scripts
# model (optional, vLLM on a GPU node): put VLLM_API_KEY in the Secret (a long random value), download the weights once with deploy/k8s/vllm-fetch-job.yaml
# (its header says how; Gemma needs an HF_TOKEN), then: kubectl -n claimguard scale deploy/vllm --replicas=1 ; set QUEUE_AI_PROVIDER=vllm and VLLM_MODEL in the ConfigMap and restart api and worker
```
The `.env` must hold `MONGO_ROOT_USER`, `MONGO_ROOT_PASSWORD` and `REDIS_PASSWORD` as well as the application secrets, because the pods read them from the one Secret. Add a second administrator early: dangerous administrator actions need two (`docs/40` section 5b).

## What is enforced by the manifests
- Namespace labelled `pod-security.kubernetes.io/enforce: restricted`: no privileged pods, no root, no host access, seccomp on.
- API and worker: non-root (65532), read-only root filesystem, all capabilities dropped, no service-account token, CPU and memory limits, probes.
- **Network policy: deny all, then allow**: only the ingress controller reaches the API; only the API and worker (`tier: app`) reach MongoDB, Redis and the model; the data tier accepts connections only from the app tier and has no egress except DNS. A compromised proxy cannot reach the database, and a compromised database cannot call out.
- The worker is a single pod with `Recreate`, scheduled on the API's node because both write the same `ReadWriteOnce` volume.
- Only the ingress is exposed; the audit mirror volume is mounted on the worker alone.

## Limits, stated honestly
- **Not yet applied to a cluster.** It is checked by a structural test (`tests/test_deploy_k8s.py`: the security promises above, parsed from the file) and was read against the Compose file; there was no cluster on the build machine. Run the checks in `docs/40` section 9 (and a port scan from outside) on the real cluster.
- **One API pod.** The audit logs are files with file locks; two API pods need a `ReadWriteMany` volume whose locking you have tested, or the logs moved into the database (not built).
- **One MongoDB member.** A three-member replica set (and the operator or chart you prefer) is the next step for high availability; backups follow `deploy/rust/backup.sh` (run it from a pod that can reach MongoDB, or use your cluster's volume snapshots).
- `TRUSTED_PROXY_IPS` is `*` in the ConfigMap because the controller's address depends on the cluster; set it to the controller's pod network.
- The Compose stack is the one covered by the deployment attack test (`rust/tools/deploy_attack.py`, 50 checks) and the backup/restore test; the Kubernetes files are not.
