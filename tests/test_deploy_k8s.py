"""The Kubernetes manifests (deploy/k8s/claimguard.yaml) keep the promises the Compose stack makes: restricted pods, a deny-all network with
only the listed paths, one worker, no secret values in the file, third-party images pinned by digest. There was no cluster to apply them to,
so these tests parse the file and check the promises directly."""
import re
import unittest
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parents[1]
TEXT = (ROOT / "deploy" / "k8s" / "claimguard.yaml").read_text(encoding="utf-8")
DOCS = [d for d in yaml.safe_load_all(TEXT) if d]


def of(kind, name=None):
    return [d for d in DOCS if d["kind"] == kind and (name is None or d["metadata"]["name"] == name)]


def pods():
    for d in DOCS:
        if d["kind"] in ("Deployment", "StatefulSet"):
            yield d["metadata"]["name"], d["spec"]["template"]["spec"]


class Manifests(unittest.TestCase):
    def test_every_document_has_the_claimguard_namespace(self):
        for d in DOCS:
            if d["kind"] != "Namespace":
                self.assertEqual(d["metadata"].get("namespace"), "claimguard", d["metadata"]["name"])

    def test_the_namespace_enforces_the_restricted_pod_profile(self):
        self.assertEqual(of("Namespace")[0]["metadata"]["labels"]["pod-security.kubernetes.io/enforce"], "restricted")

    def test_every_pod_meets_the_restricted_profile(self):
        for name, spec in pods():
            self.assertIs(spec["securityContext"].get("runAsNonRoot"), True, name)
            self.assertEqual(spec["securityContext"]["seccompProfile"]["type"], "RuntimeDefault", name)
            self.assertIs(spec.get("automountServiceAccountToken"), False, name)
            for c in spec["containers"]:
                sc = c["securityContext"]
                self.assertIs(sc["allowPrivilegeEscalation"], False, name)
                self.assertEqual(sc["capabilities"]["drop"], ["ALL"], name)
                self.assertIn("memory", c["resources"]["limits"], name)

    def test_the_application_pods_are_non_root_with_read_only_root_filesystems(self):
        for name, spec in pods():
            if name in ("api", "worker"):
                self.assertEqual(spec["securityContext"]["runAsUser"], 65532)
                self.assertIs(spec["containers"][0]["securityContext"]["readOnlyRootFilesystem"], True)

    def test_exactly_one_worker_replaced_never_overlapped(self):
        w = of("Deployment", "worker")[0]["spec"]
        self.assertEqual((w["replicas"], w["strategy"]["type"]), (1, "Recreate"))
        self.assertEqual(of("Deployment", "api")[0]["spec"]["replicas"], 1, "the audit logs are files: one API pod")

    def test_the_audit_mirror_is_mounted_on_the_worker_only(self):
        mounts = {n: [m["mountPath"] for c in s["containers"] for m in c.get("volumeMounts", [])] for n, s in pods()}
        self.assertIn("/mirror", mounts["worker"])
        for name, m in mounts.items():
            if name != "worker":
                self.assertNotIn("/mirror", m, name)

    def test_the_network_denies_everything_then_allows_only_the_listed_paths(self):
        deny = of("NetworkPolicy", "default-deny")[0]["spec"]
        self.assertEqual((deny["podSelector"], sorted(deny["policyTypes"])), ({}, ["Egress", "Ingress"]))
        into_data = of("NetworkPolicy", "data-tier-only-from-app")[0]["spec"]
        self.assertEqual(into_data["podSelector"]["matchLabels"], {"tier": "data"})
        self.assertEqual(into_data["ingress"], [{"from": [{"podSelector": {"matchLabels": {"tier": "app"}}}]}])
        ports = sorted(p["port"] for r in of("NetworkPolicy", "app-to-data")[0]["spec"]["egress"] for p in r["ports"])
        self.assertEqual(ports, [6379, 11434, 27017])
        for d in of("NetworkPolicy"):  # the data tier is granted no egress rule of its own
            if d["spec"]["podSelector"].get("matchLabels", {}).get("tier") == "data":
                self.assertNotIn("Egress", d["spec"]["policyTypes"])

    def test_only_the_ingress_controller_reaches_the_api(self):
        spec = of("NetworkPolicy", "ingress-controller-to-api")[0]["spec"]
        self.assertEqual(spec["podSelector"]["matchLabels"], {"app": "api"})
        self.assertEqual(spec["ingress"][0]["from"][0]["namespaceSelector"]["matchLabels"]["kubernetes.io/metadata.name"], "ingress-nginx")

    def test_nothing_is_exposed_outside_the_cluster_except_through_the_ingress(self):
        for s in of("Service"):
            self.assertNotIn(s["spec"].get("type", "ClusterIP"), ("NodePort", "LoadBalancer"), s["metadata"]["name"])
        ing = of("Ingress")[0]
        self.assertTrue(ing["spec"]["tls"])
        self.assertEqual(ing["spec"]["rules"][0]["http"]["paths"][0]["backend"]["service"]["name"], "api")
        self.assertEqual(ing["metadata"]["annotations"]["nginx.ingress.kubernetes.io/proxy-body-size"], "100k")

    def test_third_party_images_are_pinned_by_digest_and_no_secret_value_is_in_the_file(self):
        for name, spec in pods():
            for c in spec["containers"]:
                if not c["image"].startswith("claimguard-rs"):
                    self.assertRegex(c["image"], r"@sha256:[0-9a-f]{64}$", name)
        self.assertEqual(of("Secret"), [], "secrets are created from the .env, never written in this file")
        self.assertIsNone(re.search(r"(?i)(password|secret|key)\s*:\s*[A-Za-z0-9+/=_-]{16,}\s*$", TEXT, re.M))

    def test_settings_match_the_compose_stack(self):
        compose = (ROOT / "deploy" / "rust" / "compose.yml").read_text(encoding="utf-8")
        cm = of("ConfigMap", "claimguard-settings")[0]["data"]
        for key in ("DUAL_CONTROL", "APPROVAL_TTL_SECONDS", "AUDIT_MIRROR_DIR", "TRUSTED_PROXY_IPS"):
            self.assertIn(key + ":", compose, key)
            self.assertIn(key, cm, key)
        self.assertEqual(cm["DUAL_CONTROL"], "true")


if __name__ == "__main__":
    unittest.main()
