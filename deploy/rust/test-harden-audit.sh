# Proves harden-audit.sh in a throwaway container: docker run --rm --cap-add LINUX_IMMUTABLE -v "$PWD/deploy/rust:/h:ro" debian:bookworm-slim bash /h/test-harden-audit.sh
set -u
apt-get -qq update >/dev/null 2>&1; apt-get -qq install -y e2fsprogs >/dev/null 2>&1
d=/data; mkdir -p $d; cd $d
echo '{"a":1}' > security_audit.jsonl
bash /h/harden-audit.sh apply $d
bash /h/harden-audit.sh status $d; echo "status exit=$?"
echo '{"a":2}' >> security_audit.jsonl && echo "APPEND ok"
: > security_audit.jsonl 2>&1 && echo "TRUNCATE succeeded (BAD)" || echo "truncate refused"
echo x > security_audit.jsonl 2>&1 && echo "OVERWRITE succeeded (BAD)" || echo "overwrite refused"
mv security_audit.jsonl x.jsonl 2>&1 && echo "RENAME succeeded (BAD)" || echo "rename refused"
rm -f security_audit.jsonl 2>&1; [ -e security_audit.jsonl ] && echo "delete refused" || echo "DELETE succeeded (BAD)"
wc -l security_audit.jsonl
bash /h/harden-audit.sh lift $d; : > security_audit.jsonl && echo "after lift truncate ok"
