"""Downloads model weights into the model volume (HF_HOME). install.sh runs it once, in the `vllm-fetch` container: the only container of the
stack with a route to the internet. The model servers themselves run offline (HF_HUB_OFFLINE=1) on networks with no route out.

Environment: FETCH_MODELS (space-separated Hugging Face repo ids), HF_HOME (where the cache lives), HF_TOKEN (needed for a gated model such as
Gemma: accept its licence on huggingface.co with the same account first), VLLM_MODEL_REVISION (optional commit hash that pins the first model).
Exit code 0 only if every model is complete on disk.
"""
import os
import sys

from huggingface_hub import snapshot_download
from huggingface_hub.utils import GatedRepoError, HfHubHTTPError, RepositoryNotFoundError

# weights, tokenizer and configuration; not the original PyTorch or GGUF copies that some repositories also hold
PATTERNS = ["*.json", "*.safetensors", "*.model", "*.txt", "*.tiktoken", "*.jinja"]


def main() -> int:
    models = [m for m in os.environ.get("FETCH_MODELS", "").split() if m]
    if not models:
        print("FETCH_MODELS is empty: nothing to download")
        return 2
    token = os.environ.get("HF_TOKEN") or None
    revision = os.environ.get("VLLM_MODEL_REVISION") or None
    for i, repo in enumerate(models):
        print(f"downloading {repo}" + (f" at revision {revision}" if revision and i == 0 else "") + " ...", flush=True)
        try:
            path = snapshot_download(repo, revision=revision if i == 0 else None, token=token, allow_patterns=PATTERNS)
        except GatedRepoError:
            print(f"{repo} is gated: accept its licence on huggingface.co with your account, then run again with HF_TOKEN set to a read token")
            return 1
        except RepositoryNotFoundError:
            print(f"{repo} was not found (check the name; a private repository needs HF_TOKEN)")
            return 1
        except HfHubHTTPError as e:
            print(f"the download of {repo} failed: {e}")
            return 1
        files = [os.path.join(d, f) for d, _, fs in os.walk(path) for f in fs]
        weights = [f for f in files if f.endswith(".safetensors")]
        if not weights:
            print(f"{repo}: no .safetensors weights were found in {path}: vLLM cannot serve this repository from here")
            return 1
        size = sum(os.path.getsize(f) for f in weights)
        print(f"{repo}: {len(weights)} weight files, {size / 1e9:.1f} GB, in {path}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
