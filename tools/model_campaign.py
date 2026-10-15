#!/usr/bin/env python3
"""Run resumable OpenRouter repair-model campaigns outside the Git dataset.

Code, schemas, and mock tests live in this repository. Catalog snapshots, raw
model patches, per-request records, and reports default to `.aitex/benchmarks`,
which is intentionally ignored. A public result becomes a conscious export,
not an accidental side effect of running a benchmark.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import platform
import random
import shutil
import stat
import statistics
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from collections import defaultdict
from collections.abc import Mapping
from dataclasses import dataclass
from decimal import Decimal, InvalidOperation, ROUND_CEILING
from pathlib import Path
from typing import Any

CAMPAIGN_SCHEMA = "aitex-model-campaign-v1"
RESULT_SCHEMA = "aitex-model-campaign-result-v1"
SUMMARY_SCHEMA = "aitex-model-campaign-summary-v1"
LABEL_SCHEMA = "aitex-model-campaign-label-v1"
RESCORE_SCHEMA = "aitex-model-campaign-rescore-v1"
RESCORE_RESULT_SCHEMA = "aitex-model-campaign-rescore-result-v1"
REPAIR_SCORE_SCHEMA = "aitex-repair-score-v1"
REPAIR_PROTOCOL = "aitex-repair-protocol-v1.3"
MODELS_API = "https://openrouter.ai/api/v1/models"
MAX_SNAPSHOT_BYTES = 32 * 1024 * 1024
MAX_CREDENTIAL_FILE_BYTES = 64 * 1024
PROFILE_INPUT_CEILINGS = {"diagnostic": 300, "compact": 400, "expanded": 750}
ZERO_COMPLETION_FAILURES = {"routing-failure", "provider-refusal"}


@dataclass(frozen=True)
class Candidate:
    model: str
    provider: str | None


def openrouter_config_path(environment: Mapping[str, str] = os.environ) -> Path | None:
    xdg = environment.get("XDG_CONFIG_HOME")
    if xdg and Path(xdg).is_absolute():
        return Path(xdg) / "aitex" / "env"
    home = environment.get("HOME")
    return Path(home) / ".config" / "aitex" / "env" if home else None


def parse_secret_value(value: str, line_number: int) -> str:
    value = value.strip()
    if len(value) >= 2 and value[0] in "'\"" and value[-1] == value[0]:
        value = value[1:-1]
    elif value.startswith(("'", '"')) or value.endswith(("'", '"')):
        raise ValueError(
            f"OpenRouter credential file line {line_number} has unmatched quotes"
        )
    if not value:
        raise ValueError(
            f"OpenRouter credential file line {line_number} has an empty key"
        )
    if any(character.isspace() for character in value):
        raise ValueError(
            f"OpenRouter credential file line {line_number} has whitespace inside the key"
        )
    return value


def parse_openrouter_env(contents: str) -> str:
    key = None
    for line_number, source_line in enumerate(contents.splitlines(), 1):
        line = source_line.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("export") and line[6:7].isspace():
            line = line[6:].lstrip()
        if "=" not in line:
            continue
        name, value = line.split("=", 1)
        if name.strip() != "OPENROUTER_API_KEY":
            continue
        if key is not None:
            raise ValueError(
                "OpenRouter credential file defines OPENROUTER_API_KEY more than once"
            )
        key = parse_secret_value(value, line_number)
    if key is None:
        raise ValueError(
            "OpenRouter credential file does not define OPENROUTER_API_KEY"
        )
    return key


def read_openrouter_config(path: Path) -> str:
    try:
        with path.open("rb") as source:
            metadata = os.fstat(source.fileno())
            if not stat.S_ISREG(metadata.st_mode):
                raise ValueError(
                    f"OpenRouter credential path is not a regular file: {path}"
                )
            mode = stat.S_IMODE(metadata.st_mode)
            if mode & 0o077:
                raise ValueError(
                    f"refusing OpenRouter credential file {path} with permissions "
                    f"{mode:04o}; run chmod 600 '{path}'"
                )
            data = source.read(MAX_CREDENTIAL_FILE_BYTES + 1)
    except FileNotFoundError:
        raise
    except OSError as error:
        raise ValueError(f"cannot read OpenRouter credential file {path}: {error}") from error
    if len(data) > MAX_CREDENTIAL_FILE_BYTES:
        raise ValueError(
            f"OpenRouter credential file exceeds {MAX_CREDENTIAL_FILE_BYTES} bytes: {path}"
        )
    try:
        contents = data.decode("utf-8")
    except UnicodeDecodeError as error:
        raise ValueError(
            f"OpenRouter credential file is not valid UTF-8: {path}"
        ) from error
    return parse_openrouter_env(contents)


def load_openrouter_api_key() -> str | None:
    if "OPENROUTER_API_KEY" in os.environ:
        key = os.environ["OPENROUTER_API_KEY"]
        if not key.strip():
            raise ValueError("OPENROUTER_API_KEY is empty")
        return key
    path = openrouter_config_path()
    if path is None:
        return None
    try:
        key = read_openrouter_config(path)
    except FileNotFoundError:
        return None
    os.environ["OPENROUTER_API_KEY"] = key
    return key


def canonical(value: Any) -> bytes:
    return json.dumps(value, sort_keys=True, separators=(",", ":")).encode()


def digest(value: Any) -> str:
    return hashlib.sha256(canonical(value)).hexdigest()


def file_digest(path: Path) -> str:
    hasher = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            hasher.update(block)
    return hasher.hexdigest()


def tool_facts(name: str) -> dict[str, Any]:
    found = shutil.which(name)
    if not found:
        return {"name": name, "available": False}
    path = Path(found).resolve()
    try:
        process = subprocess.run(
            [str(path), "--version"],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            timeout=5,
            check=False,
        )
        version = next(
            (line.strip() for line in process.stdout.splitlines() if line.strip()), None
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        version = f"unavailable: {error}"
    return {
        "name": name,
        "available": True,
        "path": str(path),
        "sha256": file_digest(path) if path.is_file() else None,
        "version": version,
    }


def atomic_write(path: Path, data: bytes, mode: int = 0o600) -> None:
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    descriptor, temporary = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    try:
        os.fchmod(descriptor, mode)
        with os.fdopen(descriptor, "wb") as output:
            output.write(data)
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, path)
    except BaseException:
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass
        raise


def append_jsonl(path: Path, value: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o600)
    try:
        with os.fdopen(descriptor, "ab", closefd=True) as output:
            output.write(canonical(value) + b"\n")
            output.flush()
            os.fsync(output.fileno())
    except BaseException:
        try:
            os.close(descriptor)
        except OSError:
            pass
        raise


def read_json(path: Path) -> dict[str, Any]:
    value = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(value, dict):
        raise ValueError(f"expected a JSON object: {path}")
    return value


def read_results(
    path: Path, schema: str = RESULT_SCHEMA
) -> list[dict[str, Any]]:
    if not path.is_file():
        return []
    records = []
    seen: set[str] = set()
    with path.open(encoding="utf-8") as source:
        for line_number, line in enumerate(source, 1):
            if not line.strip():
                continue
            value = json.loads(line)
            if value.get("schema") != schema:
                raise ValueError(f"wrong result schema at {path}:{line_number}")
            job_id = value.get("job_id")
            if not isinstance(job_id, str) or job_id in seen:
                raise ValueError(f"missing or duplicate job id at {path}:{line_number}")
            seen.add(job_id)
            records.append(value)
    return records


def read_labels(path: Path) -> dict[str, dict[str, Any]]:
    if not path.is_file():
        return {}
    labels = {}
    with path.open(encoding="utf-8") as source:
        for line_number, line in enumerate(source, 1):
            if not line.strip():
                continue
            value = json.loads(line)
            if value.get("schema") != LABEL_SCHEMA or value.get("label") not in (
                "correct",
                "incorrect",
                "uncertain",
            ):
                raise ValueError(f"invalid semantic label at {path}:{line_number}")
            labels[value["job_id"]] = value
    return labels


def fetch_snapshot(api_key: str | None) -> bytes:
    headers = {"User-Agent": "aiTeX/0.3"}
    if api_key:
        headers["Authorization"] = f"Bearer {api_key}"
    request = urllib.request.Request(
        MODELS_API,
        headers=headers,
    )
    try:
        with urllib.request.urlopen(request, timeout=60) as response:
            data = response.read(MAX_SNAPSHOT_BYTES + 1)
    except urllib.error.HTTPError as error:
        detail = error.read(4096).decode("utf-8", errors="replace")
        raise ValueError(f"OpenRouter models API returned {error.code}: {detail}") from error
    except urllib.error.URLError as error:
        raise ValueError(f"cannot fetch OpenRouter model catalog: {error}") from error
    if len(data) > MAX_SNAPSHOT_BYTES:
        raise ValueError("OpenRouter model catalog exceeds the 32 MiB safety limit")
    value = json.loads(data)
    if not isinstance(value, dict) or not isinstance(value.get("data"), list):
        raise ValueError("OpenRouter model catalog has no data array")
    return canonical(value) + b"\n"


def load_snapshot(path: Path) -> dict[str, dict[str, Any]]:
    value = read_json(path)
    rows = value.get("data")
    if not isinstance(rows, list):
        raise ValueError(f"snapshot has no data array: {path}")
    models = {
        row["id"]: row
        for row in rows
        if isinstance(row, dict) and isinstance(row.get("id"), str)
    }
    if not models:
        raise ValueError(f"snapshot contains no model identities: {path}")
    return models


def decimal_field(value: Any, label: str) -> Decimal:
    try:
        number = Decimal(str(value))
    except (InvalidOperation, ValueError) as error:
        raise ValueError(f"invalid {label} price {value!r}") from error
    if not number.is_finite() or number < 0:
        raise ValueError(f"invalid {label} price {value!r}")
    return number


def prices(model: dict[str, Any]) -> tuple[Decimal, Decimal, Decimal]:
    pricing = model.get("pricing")
    if not isinstance(pricing, dict):
        raise ValueError(f"model {model.get('id')} has no pricing")
    return (
        decimal_field(pricing.get("prompt"), "prompt"),
        decimal_field(pricing.get("completion"), "completion"),
        decimal_field(pricing.get("request", 0), "request"),
    )


def reserve_microusd(
    model: dict[str, Any], max_input_tokens: int, max_output_tokens: int
) -> int:
    prompt, completion, request = prices(model)
    pricing = model["pricing"]
    cache_rates = [prompt]
    for field in ("input_cache_read", "input_cache_write"):
        if pricing.get(field) is not None:
            cache_rates.append(decimal_field(pricing[field], field))
    reasoning = decimal_field(pricing.get("internal_reasoning", 0), "internal_reasoning")
    # Reasoning token billing varies by model. Charging both output classes is
    # intentionally pessimistic; an unused reservation costs nothing.
    dollars = (
        max(cache_rates) * max_input_tokens
        + (completion + reasoning) * max_output_tokens
        + request
    )
    return int((dollars * Decimal(1_000_000)).to_integral_value(rounding=ROUND_CEILING))


def reservation_input_tokens(arguments: argparse.Namespace, job: dict[str, Any]) -> int:
    # The lab's estimate is ceil(rendered UTF-8 bytes / 4). Four times its
    # enforced profile ceiling therefore covers even a one-token-per-byte
    # tokenizer while retaining the caller's absolute safety ceiling.
    return min(arguments.max_input_tokens, PROFILE_INPUT_CEILINGS[job["profile"]] * 4)


def budget_debit(record: dict[str, Any]) -> int:
    """Use authoritative cost when present, otherwise the reserved worst case."""
    cost = record["score"].get("cost_microusd")
    if isinstance(cost, int):
        return cost
    reservation = record.get("reservation_microusd")
    if not isinstance(reservation, int) or reservation < 0:
        raise ValueError("an unaccounted result has no valid budget reservation")
    return reservation


def per_million(price_per_token: Decimal) -> str:
    return format(price_per_token * Decimal(1_000_000), "f")


def model_cases(corpus: Path) -> list[str]:
    manifest = read_json(corpus)
    if manifest.get("schema") != "aitex-repair-corpus-v1":
        raise ValueError(f"unexpected repair corpus schema: {corpus}")
    cases = [
        case["id"]
        for case in manifest.get("cases", [])
        if isinstance(case, dict)
        and case.get("expected_route") == "model"
        and isinstance(case.get("id"), str)
    ]
    if not cases:
        raise ValueError("repair corpus has no model-routed cases")
    return cases


def parse_candidates(
    specs: list[str], track: str, default_provider: str | None
) -> list[Candidate]:
    result = []
    for spec in specs:
        model, separator, provider = spec.partition("=")
        if not model or (separator and not provider):
            raise ValueError(f"invalid model candidate {spec!r}")
        selected = provider if separator else default_provider
        if track == "pinned" and not selected:
            raise ValueError(
                "pinned campaigns require MODEL=PROVIDER or --openrouter-provider"
            )
        if track == "routed" and selected:
            raise ValueError("explicit providers belong only to the pinned track")
        result.append(Candidate(model, selected))
    if len({(item.model, item.provider) for item in result}) != len(result):
        raise ValueError("duplicate model candidate")
    return result


def job_seed(campaign_seed: int, payload: dict[str, Any]) -> int:
    raw = hashlib.sha256(str(campaign_seed).encode() + canonical(payload)).digest()
    return int.from_bytes(raw[:8], "big") & ((1 << 63) - 1)


def build_jobs(
    candidates: list[Candidate],
    cases: list[str],
    profiles: list[str],
    repeats: int,
    campaign_seed: int,
) -> list[dict[str, Any]]:
    jobs = []
    for candidate in candidates:
        for case in cases:
            for profile in profiles:
                for repeat in range(1, repeats + 1):
                    identity = {
                        "model": candidate.model,
                        "requested_provider": candidate.provider,
                        "case": case,
                        "profile": profile,
                        "repeat": repeat,
                        "campaign_seed": campaign_seed,
                    }
                    jobs.append(
                        {
                            **identity,
                            "seed": job_seed(campaign_seed, identity),
                            "job_id": digest(identity),
                        }
                    )
    random.Random(campaign_seed).shuffle(jobs)
    for order, job in enumerate(jobs, 1):
        job["order"] = order
    return jobs


def campaign_configuration(arguments: argparse.Namespace, candidates: list[Candidate]) -> dict[str, Any]:
    return {
        "repair_protocol": REPAIR_PROTOCOL,
        "corpus": str(arguments.corpus.resolve()),
        "corpus_sha256": file_digest(arguments.corpus),
        "candidates": [item.__dict__ for item in candidates],
        "profiles": arguments.profiles,
        "repeats": arguments.repeats,
        "track": arguments.track,
        "seed": arguments.seed,
        "max_input_tokens": arguments.max_input_tokens,
        "max_output_tokens": arguments.max_output_tokens,
        "budget_microusd": arguments.budget_microusd,
        "retries": arguments.retries,
        "min_request_ms": arguments.min_request_ms,
        "aitex": str(arguments.aitex.resolve()),
        "aitex_sha256": file_digest(arguments.aitex),
        "lab": str(arguments.lab.resolve()),
        "lab_sha256": file_digest(arguments.lab),
        "runner_sha256": file_digest(Path(__file__).resolve()),
        "environment": {
            "platform": platform.platform(),
            "python": platform.python_version(),
            "semantic_tools": [
                tool_facts(name)
                for name in ("kpsewhich", "bibtex", "biber", "makeindex", "makeglossaries")
            ],
        },
        "privacy": {"data_collection": "deny", "zdr": True},
        "ignored_providers": arguments.openrouter_ignore_provider,
    }


def create_or_resume_campaign(
    arguments: argparse.Namespace, candidates: list[Candidate], jobs: list[dict[str, Any]]
) -> tuple[dict[str, Any], Path]:
    directory = arguments.campaign.resolve()
    directory.mkdir(parents=True, exist_ok=True, mode=0o700)
    try:
        os.chmod(directory, 0o700)
    except OSError:
        pass
    snapshot = directory / "models.json"
    if not snapshot.exists():
        if arguments.snapshot:
            data = arguments.snapshot.read_bytes()
            value = json.loads(data)
            if not isinstance(value, dict) or not isinstance(value.get("data"), list):
                raise ValueError("supplied model snapshot has no data array")
            atomic_write(snapshot, canonical(value) + b"\n")
        else:
            # The models catalog is public. Do not read or transmit a private
            # credential merely to create a no-cost campaign plan.
            atomic_write(snapshot, fetch_snapshot(None))
    configuration = campaign_configuration(arguments, candidates)
    manifest_path = directory / "campaign.json"
    if manifest_path.exists():
        manifest = read_json(manifest_path)
        if manifest.get("schema") != CAMPAIGN_SCHEMA:
            raise ValueError("campaign directory has an incompatible manifest")
        if manifest.get("configuration") != configuration:
            raise ValueError("campaign configuration changed; use a new campaign directory")
        if manifest.get("models_sha256") != file_digest(snapshot):
            raise ValueError("frozen model snapshot changed")
    else:
        manifest = {
            "schema": CAMPAIGN_SCHEMA,
            "created_unix_ms": int(time.time() * 1000),
            "configuration": configuration,
            "models_sha256": file_digest(snapshot),
            "jobs": jobs,
        }
        atomic_write(manifest_path, json.dumps(manifest, indent=2, sort_keys=True).encode() + b"\n")
    return manifest, snapshot


def failure_class(error: str | None, score: dict[str, Any] | None) -> str | None:
    if score:
        requested_limit = score.get("max_output_tokens")
        observed_output = score.get("output_tokens")
        if (
            isinstance(requested_limit, int)
            and isinstance(observed_output, int)
            and observed_output > requested_limit
        ):
            return "provider-output-limit-violation"
    if not error and score and score.get("build_passed"):
        return None
    text = (error or "").lower()
    if "campaign protocol mismatch" in text:
        return "protocol-mismatch"
    if "api error 429" in text:
        return "rate-limit"
    if "api error 404" in text:
        return "routing-failure"
    if any(f"api error {status}" in text for status in (400, 401, 402, 403, 405, 409, 422)):
        return "provider-refusal"
    if "api error 408" in text:
        return "timeout"
    if any(f"api error {status}" in text for status in range(500, 600)):
        return "provider-failure"
    if "transport error" in text or "timed out" in text or "timeout" in text:
        return "transport"
    if score and score.get("response_received") and not score.get("response_contract_valid"):
        return "invalid-response-contract"
    if score and score.get("response_contract_valid") and not score.get("proposal_nonempty"):
        return "empty-proposal"
    if score and score.get("proposal_nonempty") and not score.get("target_matched"):
        return "unresolved-target"
    if score and score.get("target_matched") and not score.get("patch_accepted"):
        return "ungrounded-edit"
    if "model response:" in text or "returned no text" in text:
        return "malformed-output"
    if "unsafe patch" in text:
        return "unsafe-patch"
    if score and score.get("patch_accepted") and not score.get("build_passed"):
        return "compile-failure"
    if any(marker in text for marker in ("patch", "unified-diff", "hunk header")):
        return "invalid-patch"
    return "unknown"


def account_zero_completion_insurance(
    score: dict[str, Any], category: str | None
) -> None:
    """Account explicit pre-generation API rejections as documented zero cost.

    OpenRouter's zero-completion insurance covers responses with an error and
    no output tokens. Keep ambiguous transport, server, timeout, and rate-limit
    failures outside this rule so their measured charge remains explicitly
    unknown and the full request reservation is debited instead.
    """
    if score.get("cost_microusd") is None and category in ZERO_COMPLETION_FAILURES:
        score["cost_microusd"] = 0
        score["cost_basis"] = "openrouter-zero-completion-insurance"


EVALUATION_FIELDS = (
    "tier",
    "surface",
    "answer_cause",
    "answer_confidence",
    "proposal_format",
    "structured_edits",
    "response_normalization",
    "patch",
    "normalized_patch",
    "patch_normalization",
    "response_received",
    "response_contract_valid",
    "proposal_nonempty",
    "patch_accepted",
    "safety_passed",
    "changed_lines",
    "deleted_lines",
    "target_matched",
    "build_passed",
    "oracle_matched",
    "validation_ms",
    "error",
)


def merge_rescored_score(
    original: dict[str, Any], replayed: dict[str, Any]
) -> dict[str, Any]:
    """Replace evaluator conclusions without rewriting provider evidence."""
    merged = dict(original)
    for field in EVALUATION_FIELDS:
        merged[field] = replayed.get(field)
    return merged


def single_score(path: Path) -> dict[str, Any]:
    lines = [line for line in path.read_text(encoding="utf-8").splitlines() if line]
    if len(lines) != 1:
        raise ValueError(f"offline replay produced {len(lines)} score rows, expected one")
    value = json.loads(lines[0])
    if not isinstance(value, dict) or value.get("schema") != REPAIR_SCORE_SCHEMA:
        raise ValueError("offline replay produced an incompatible score row")
    return value


def copy_frozen_asset(source: Path, target: Path) -> None:
    data = source.read_bytes()
    if target.exists():
        if target.read_bytes() != data:
            raise ValueError(f"frozen rescore asset changed: {target}")
        return
    atomic_write(target, data)


def rescore_campaign(arguments: argparse.Namespace) -> tuple[Path, dict[str, Any]]:
    campaign = arguments.campaign.resolve()
    source_manifest_path = campaign / "campaign.json"
    source_results_path = campaign / "results.jsonl"
    source_manifest = read_json(source_manifest_path)
    if source_manifest.get("schema") != CAMPAIGN_SCHEMA:
        raise ValueError("campaign directory has an incompatible manifest")
    source_records = read_results(source_results_path)
    jobs = source_manifest.get("jobs")
    if not isinstance(jobs, list) or {row.get("job_id") for row in source_records} != {
        job.get("job_id") for job in jobs
    }:
        raise ValueError("offline re-scoring requires one result for every campaign job")
    if not arguments.lab.is_file():
        raise ValueError(
            f"repair-lab binary is missing: {arguments.lab}; run make repair-lab"
        )
    if not arguments.aitex.is_file():
        raise ValueError(f"aiTeX binary is missing: {arguments.aitex}; run make")
    if not arguments.corpus.is_file():
        raise ValueError(f"repair corpus is missing: {arguments.corpus}")
    source_corpus_digest = source_manifest.get("configuration", {}).get("corpus_sha256")
    if file_digest(arguments.corpus) != source_corpus_digest:
        raise ValueError("repair corpus differs from the campaign's frozen corpus")

    labels_path = campaign / "labels.jsonl"
    identity = {
        "source_campaign_sha256": file_digest(source_manifest_path),
        "source_results_sha256": file_digest(source_results_path),
        "source_labels_sha256": file_digest(labels_path) if labels_path.is_file() else None,
        "models_sha256": file_digest(campaign / "models.json"),
        "corpus_sha256": file_digest(arguments.corpus),
        "lab_sha256": file_digest(arguments.lab),
        "aitex_sha256": file_digest(arguments.aitex),
        "runner_sha256": file_digest(Path(__file__).resolve()),
    }
    evaluator_id = digest(identity)
    directory = campaign / "rescores" / evaluator_id[:16]
    directory.mkdir(parents=True, exist_ok=True, mode=0o700)
    try:
        os.chmod(directory, 0o700)
    except OSError:
        pass
    manifest_path = directory / "rescore.json"
    if manifest_path.exists():
        rescore_manifest = read_json(manifest_path)
        if (
            rescore_manifest.get("schema") != RESCORE_SCHEMA
            or rescore_manifest.get("evaluator_id") != evaluator_id
            or rescore_manifest.get("identity") != identity
        ):
            raise ValueError("content-addressed rescore manifest changed")
    else:
        rescore_manifest = {
            "schema": RESCORE_SCHEMA,
            "evaluator_id": evaluator_id,
            "created_unix_ms": int(time.time() * 1000),
            "source_campaign": str(campaign),
            "planned_jobs": len(source_records),
            "identity": identity,
        }
        atomic_write(
            manifest_path,
            json.dumps(rescore_manifest, indent=2, sort_keys=True).encode() + b"\n",
        )
    copy_frozen_asset(campaign / "models.json", directory / "models.json")
    if labels_path.is_file():
        copy_frozen_asset(labels_path, directory / "labels.jsonl")

    result_path = directory / "results.jsonl"
    rescored = read_results(result_path, RESCORE_RESULT_SCHEMA)
    completed = {record["job_id"] for record in rescored}
    with tempfile.TemporaryDirectory(prefix="aitex-offline-rescore-", dir=directory) as raw:
        temporary = Path(raw)
        for position, original in enumerate(source_records, 1):
            if original["job_id"] in completed:
                continue
            old_score = original.get("score")
            if not isinstance(old_score, dict):
                raise ValueError(f"campaign result {original['job_id']} has no score object")
            raw_response = old_score.get("raw_response")
            if not isinstance(raw_response, str):
                raise ValueError(
                    f"campaign result {original['job_id']} has no replayable raw response"
                )
            response_path = temporary / "response.txt"
            score_path = temporary / "score.jsonl"
            atomic_write(response_path, raw_response.encode())
            score_path.unlink(missing_ok=True)
            command = [
                str(arguments.lab.resolve()),
                "--corpus",
                str(arguments.corpus.resolve()),
                "--provider",
                "recording",
                "--replay-response",
                str(response_path),
                "--case",
                str(old_score.get("case_id")),
                "--profile",
                str(old_score.get("profile")),
                "--max-output-tokens",
                str(old_score.get("max_output_tokens") or 4096),
                "--score",
                "--results",
                str(score_path),
                "--aitex",
                str(arguments.aitex.resolve()),
            ]
            process = subprocess.run(
                command,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.PIPE,
                text=True,
                timeout=arguments.timeout,
                check=False,
            )
            if not score_path.is_file():
                raise ValueError(
                    f"offline replay failed for {original['job_id']}: "
                    f"{process.stderr[-4096:].strip()}"
                )
            replayed = single_score(score_path)
            merged = merge_rescored_score(old_score, replayed)
            record = dict(original)
            record.update(
                {
                    "schema": RESCORE_RESULT_SCHEMA,
                    "source_record_sha256": digest(original),
                    "original_failure_class": original.get("failure_class"),
                    "failure_class": failure_class(merged.get("error"), merged),
                    "rescored_unix_ms": int(time.time() * 1000),
                    "evaluator_id": evaluator_id,
                    "score": merged,
                }
            )
            append_jsonl(result_path, record)
            rescored.append(record)
            outcome = "build" if merged.get("build_passed") else record["failure_class"]
            print(
                f"  Replay    {position}/{len(source_records)} · "
                f"{old_score.get('requested_model')} · {old_score.get('case_id')} · {outcome}",
                flush=True,
            )

    # Do not rewrite a completed immutable result tree on an idempotent replay.
    summary_path = directory / "summary.json"
    report_path = directory / "report.md"
    if len(rescored) == len(source_records) and summary_path.is_file() and report_path.is_file():
        return directory, read_json(summary_path)
    summary = write_report(
        directory,
        source_manifest,
        rescored,
        {"schema": RESCORE_SCHEMA, "evaluator_id": evaluator_id, "identity": identity},
    )
    return directory, summary


def run_job(
    arguments: argparse.Namespace,
    job: dict[str, Any],
    model: dict[str, Any],
    campaign_dir: Path,
    reservation: int,
) -> dict[str, Any]:
    prompt_price, completion_price, _ = prices(model)
    temporary = campaign_dir / f".{job['job_id']}.score.jsonl"
    command = [
        str(arguments.lab.resolve()),
        "--corpus",
        str(arguments.corpus.resolve()),
        "--provider",
        "openrouter",
        "--model",
        job["model"],
        "--case",
        job["case"],
        "--profile",
        job["profile"],
        "--seed",
        str(job["seed"]),
        "--max-output-tokens",
        str(arguments.max_output_tokens),
        "--max-prompt-price",
        per_million(prompt_price),
        "--max-completion-price",
        per_million(completion_price),
        "--allow-source-upload",
        "--score",
        "--results",
        str(temporary),
        "--aitex",
        str(arguments.aitex.resolve()),
    ]
    if job.get("requested_provider"):
        command.extend(["--openrouter-provider", job["requested_provider"]])
    for provider in arguments.openrouter_ignore_provider:
        command.extend(["--openrouter-ignore-provider", provider])
    attempts = 0
    score = None
    stderr = ""
    exit_code = 1
    started = int(time.time() * 1000)
    while attempts <= arguments.retries:
        attempts += 1
        try:
            temporary.unlink(missing_ok=True)
            process = subprocess.run(
                command,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.PIPE,
                text=True,
                timeout=arguments.timeout,
                check=False,
            )
            exit_code = process.returncode
            stderr = process.stderr[-16_384:]
        except subprocess.TimeoutExpired as error:
            exit_code = 124
            stderr = f"campaign subprocess timeout after {arguments.timeout}s: {error}"
        if temporary.is_file():
            lines = [line for line in temporary.read_text(encoding="utf-8").splitlines() if line]
            if len(lines) == 1:
                candidate = json.loads(lines[0])
                if isinstance(candidate, dict):
                    score = candidate
        error = score.get("error") if score else stderr
        category = failure_class(error, score)
        # A 429 is known not to have generated billable output. Transport and
        # 5xx failures are not retried automatically because their billing is
        # ambiguous; doing so could violate a hard campaign budget.
        if category != "rate-limit" or attempts > arguments.retries:
            break
        time.sleep(min(30.0, arguments.backoff_seconds * (2 ** (attempts - 1))))
    temporary.unlink(missing_ok=True)
    if score is None:
        score = {
            "schema": REPAIR_SCORE_SCHEMA,
            "protocol": REPAIR_PROTOCOL,
            "case_id": job["case"],
            "profile": job["profile"],
            "provider": "openrouter",
            "requested_model": job["model"],
            "response_received": False,
            "response_contract_valid": False,
            "proposal_nonempty": False,
            "target_matched": False,
            "build_passed": False,
            "oracle_matched": False,
            "patch_accepted": False,
            "safety_passed": False,
            "cost_microusd": None,
            "error": stderr.strip() or f"repair lab exited with {exit_code}",
        }
    elif score.get("protocol") != REPAIR_PROTOCOL:
        score["error"] = (
            f"campaign protocol mismatch: runner expected {REPAIR_PROTOCOL}, "
            f"repair lab returned {score.get('protocol')!r}"
        )
    error = score.get("error")
    category = failure_class(error, score)
    account_zero_completion_insurance(score, category)
    return {
        "schema": RESULT_SCHEMA,
        "job_id": job["job_id"],
        "order": job["order"],
        "repeat": job["repeat"],
        "seed": job["seed"],
        "track": arguments.track,
        "requested_provider": job.get("requested_provider"),
        "reservation_microusd": reservation,
        "runner_attempts": attempts,
        "started_unix_ms": started,
        "finished_unix_ms": int(time.time() * 1000),
        "runner_exit_code": exit_code,
        "failure_class": category,
        "score": score,
    }


def percentile(values: list[float], fraction: float) -> float | None:
    if not values:
        return None
    ordered = sorted(values)
    index = max(0, math.ceil(fraction * len(ordered)) - 1)
    return ordered[index]


def wilson(successes: int, total: int) -> tuple[float, float]:
    if total == 0:
        return (0.0, 0.0)
    z = 1.959963984540054
    p = successes / total
    denominator = 1 + z * z / total
    center = (p + z * z / (2 * total)) / denominator
    half = z * math.sqrt((p * (1 - p) + z * z / (4 * total)) / total) / denominator
    return (max(0.0, center - half), min(1.0, center + half))


def aggregate(
    records: list[dict[str, Any]], labels: dict[str, dict[str, Any]] | None = None
) -> list[dict[str, Any]]:
    labels = labels or {}
    groups: dict[tuple[str, str, str], list[dict[str, Any]]] = defaultdict(list)
    for record in records:
        score = record["score"]
        identity = (
            score.get("requested_model") or "<unknown-model>",
            score.get("served_provider")
            or score.get("requested_provider")
            or record.get("requested_provider")
            or "<unknown-provider>",
            score.get("profile") or "<unknown-profile>",
        )
        groups[identity].append(record)
    result = []
    for (model, provider, profile), rows in sorted(groups.items()):
        scores = [row["score"] for row in rows]
        successes = sum(bool(score.get("build_passed")) for score in scores)
        exact = sum(bool(score.get("oracle_matched")) for score in scores)
        safe = sum(bool(score.get("safety_passed")) for score in scores)
        costs = [score.get("cost_microusd") for score in scores]
        known_costs = [int(cost) for cost in costs if isinstance(cost, int)]
        latencies = [
            float(score["model_ms"])
            for score in scores
            if isinstance(score.get("model_ms"), (int, float))
        ]
        successful_latencies = [
            float(score["model_ms"])
            for score in scores
            if score.get("build_passed")
            and isinstance(score.get("model_ms"), (int, float))
        ]
        first_bytes = [
            float(score["time_to_first_byte_ms"])
            for score in scores
            if isinstance(score.get("time_to_first_byte_ms"), (int, float))
        ]
        provider_times = [
            float(score["provider_generation_ms"])
            for score in scores
            if isinstance(score.get("provider_generation_ms"), (int, float))
        ]
        validation_times = [
            float(score["validation_ms"])
            for score in scores
            if isinstance(score.get("validation_ms"), (int, float))
        ]
        low, high = wilson(successes, len(rows))
        total_cost = sum(known_costs)
        semantic = [labels[row["job_id"]]["label"] for row in rows if row["job_id"] in labels]
        semantic_successes = semantic.count("correct")
        semantic_ci = wilson(semantic_successes, len(semantic)) if semantic else None
        input_tokens = sum(int(score.get("input_tokens") or 0) for score in scores)
        cache_read_tokens = sum(
            int(score.get("cache_read_tokens") or 0) for score in scores
        )
        cache_write_tokens = sum(
            int(score.get("cache_write_tokens") or 0) for score in scores
        )
        output_tokens = sum(int(score.get("output_tokens") or 0) for score in scores)
        reasoning_tokens = sum(
            int(score.get("reasoning_tokens") or 0) for score in scores
        )
        total_input_tokens = input_tokens + cache_read_tokens + cache_write_tokens
        failure_categories = [
            failure_class(row["score"].get("error"), row["score"])
            for row in rows
        ]
        result.append(
            {
                "model": model,
                "provider": provider,
                "profile": profile,
                "attempts": len(rows),
                "protocols": sorted(
                    {
                        str(score.get("protocol") or "aitex-repair-protocol-v1")
                        for score in scores
                    }
                ),
                "diagnostic_presentation_bytes_per_attempt": (
                    sum(int(score.get("diagnostic_presentation_bytes") or 0) for score in scores)
                    / len(rows)
                ),
                "diagnostic_gaps": dict(
                    sorted(
                        {
                            str(score.get("diagnostic_gap") or "unknown"): sum(
                                str(item.get("diagnostic_gap") or "unknown")
                                == str(score.get("diagnostic_gap") or "unknown")
                                for item in scores
                            )
                            for score in scores
                        }.items()
                    )
                ),
                "build_successes": successes,
                "build_rate": successes / len(rows),
                "build_rate_ci95": [low, high],
                "exact_oracle": exact,
                "safety_passes": safe,
                "semantic_labeled": len(semantic),
                "semantic_correct": semantic_successes,
                "semantic_incorrect": semantic.count("incorrect"),
                "semantic_uncertain": semantic.count("uncertain"),
                "semantic_rate": (
                    semantic_successes / len(semantic) if semantic else None
                ),
                "semantic_rate_ci95": semantic_ci,
                "charged_microusd": total_cost,
                "missing_cost_records": len(costs) - len(known_costs),
                "cost_per_attempt_microusd": (
                    total_cost / len(rows) if len(known_costs) == len(costs) else None
                ),
                "cost_per_success_microusd": (
                    total_cost / successes if successes and len(known_costs) == len(costs) else None
                ),
                "cost_per_build_success_microusd": (
                    total_cost / successes if successes and len(known_costs) == len(costs) else None
                ),
                "cost_per_semantic_success_microusd": (
                    total_cost / semantic_successes
                    if semantic_successes
                    and len(semantic) == len(rows)
                    and len(known_costs) == len(costs)
                    else None
                ),
                "input_tokens": input_tokens,
                "cache_read_tokens": cache_read_tokens,
                "cache_write_tokens": cache_write_tokens,
                "output_tokens": output_tokens,
                "reasoning_tokens": reasoning_tokens,
                "tokens_per_attempt": {
                    "input": total_input_tokens / len(rows),
                    "output": output_tokens / len(rows),
                    "reasoning": reasoning_tokens / len(rows),
                },
                "tokens_per_build_success": (
                    {
                        "input": total_input_tokens / successes,
                        "output": output_tokens / successes,
                        "reasoning": reasoning_tokens / successes,
                    }
                    if successes
                    else None
                ),
                "model_ms_p50": statistics.median(latencies) if latencies else None,
                "model_ms_p95": percentile(latencies, 0.95),
                "successful_model_ms_p50": (
                    statistics.median(successful_latencies) if successful_latencies else None
                ),
                "successful_model_ms_p95": percentile(successful_latencies, 0.95),
                "time_to_first_byte_ms_p50": (
                    statistics.median(first_bytes) if first_bytes else None
                ),
                "time_to_first_byte_ms_p95": percentile(first_bytes, 0.95),
                "provider_generation_ms_p50": (
                    statistics.median(provider_times) if provider_times else None
                ),
                "provider_generation_ms_p95": percentile(provider_times, 0.95),
                "validation_ms_p50": (
                    statistics.median(validation_times) if validation_times else None
                ),
                "validation_ms_p95": percentile(validation_times, 0.95),
                "failures": dict(
                    sorted(
                        {
                            kind: failure_categories.count(kind)
                            for kind in set(failure_categories)
                            if kind
                        }.items()
                    )
                ),
            }
        )
    return result


def pareto(groups: list[dict[str, Any]]) -> list[dict[str, str]]:
    def success_cost(group: dict[str, Any]) -> float | None:
        if group["semantic_labeled"] == group["attempts"]:
            return group["cost_per_semantic_success_microusd"]
        return group["cost_per_build_success_microusd"]

    eligible = [
        group
        for group in groups
        if success_cost(group) is not None and group["model_ms_p95"] is not None
    ]
    frontier = []
    quality = lambda group: (
        group["semantic_rate"]
        if group["semantic_labeled"] == group["attempts"]
        else group["build_rate"]
    )
    for candidate in eligible:
        dominated = any(
            other is not candidate
            and quality(other) >= quality(candidate)
            and success_cost(other) <= success_cost(candidate)
            and other["model_ms_p95"] <= candidate["model_ms_p95"]
            and (
                quality(other) > quality(candidate)
                or success_cost(other) < success_cost(candidate)
                or other["model_ms_p95"] < candidate["model_ms_p95"]
            )
            for other in eligible
        )
        if not dominated:
            frontier.append(
                {
                    "model": candidate["model"],
                    "provider": candidate["provider"],
                    "profile": candidate["profile"],
                    "quality_metric": (
                        "semantic" if candidate["semantic_labeled"] == candidate["attempts"] else "build"
                    ),
                }
            )
    return frontier


def markdown_report(summary: dict[str, Any]) -> str:
    lines = [
        "# aiTeX repair-model campaign",
        "",
        f"Completed jobs: {summary['completed_jobs']}/{summary['planned_jobs']}",
        f"Repair protocol: `{summary['configuration'].get('repair_protocol', 'aitex-repair-protocol-v1')}`",
        f"Charged cost: ${summary['charged_microusd'] / 1_000_000:.6f}",
        f"Conservative budget debit: ${summary['budget_debit_microusd'] / 1_000_000:.6f} "
        f"({summary['unknown_cost_records']} unknown-cost records)",
        f"Reservation overruns: {summary['reservation_overruns']}",
        f"Frozen model catalog: `{summary['models_sha256']}`",
    ]
    if summary.get("rescore"):
        lines.append(
            f"Offline evaluator: `{summary['rescore']['evaluator_id']}` "
            "(saved responses; no provider calls)"
        )
    funnel = summary.get("funnel", {})
    if funnel:
        lines.extend(
            [
                "",
                "Proposal funnel: "
                + " → ".join(
                    f"{label} {funnel.get(field, 0)}/{funnel.get('attempts', 0)}"
                    for field, label in (
                        ("response_received", "response"),
                        ("response_contract_valid", "contract"),
                        ("proposal_nonempty", "nonempty"),
                        ("target_matched", "target"),
                        ("patch_accepted", "grounded"),
                        ("safety_passed", "safe"),
                        ("build_passed", "build"),
                        ("oracle_matched", "oracle"),
                    )
                ),
            ]
        )
    if summary.get("tiers"):
        lines.extend(["", "Difficulty strata:"])
        for tier, counts in sorted(summary["tiers"].items()):
            lines.append(
                f"- {tier}: {counts['build_passed']}/{counts['attempts']} build; "
                f"{counts['oracle_matched']}/{counts['attempts']} exact oracle"
            )
    lines.extend([
        "",
        "Build success is a compiler gate, not a human semantic judgment.",
        "The Pareto frontier uses semantic correctness only for fully labeled groups; otherwise it is explicitly build-based.",
        "Different served providers and context profiles remain separate rows.",
        "",
        "| Model | Provider | Profile | Build | Semantic | Build 95% CI | Cost | $/attempt | $/build | $/semantic | E2E p50 | E2E p95 | Tokens in/out/reason |",
        "|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|",
    ])
    for group in summary["groups"]:
        low, high = group["build_rate_ci95"]
        per_build = group["cost_per_build_success_microusd"]
        per_semantic = group["cost_per_semantic_success_microusd"]
        per_attempt = group["cost_per_attempt_microusd"]
        lines.append(
            "| {model} | {provider} | {profile} | {success}/{attempts} | {semantic} | {low:.1%}–{high:.1%} | ${cost:.6f} | {per_attempt} | {per_build} | {per_semantic} | {p50} | {p95} | {input}/{output}/{reasoning} |".format(
                model=group["model"],
                provider=group["provider"],
                profile=group["profile"],
                success=group["build_successes"],
                attempts=group["attempts"],
                semantic=(
                    "—"
                    if group["semantic_labeled"] == 0
                    else f"{group['semantic_correct']}/{group['semantic_labeled']}"
                ),
                low=low,
                high=high,
                cost=group["charged_microusd"] / 1_000_000,
                per_attempt=(
                    "—" if per_attempt is None else f"${per_attempt / 1_000_000:.6f}"
                ),
                per_build=(
                    "—" if per_build is None else f"${per_build / 1_000_000:.6f}"
                ),
                per_semantic=(
                    "—" if per_semantic is None else f"${per_semantic / 1_000_000:.6f}"
                ),
                p50=("—" if group["model_ms_p50"] is None else f"{group['model_ms_p50']:.0f} ms"),
                p95=("—" if group["model_ms_p95"] is None else f"{group['model_ms_p95']:.0f} ms"),
                input=group["input_tokens"] + group["cache_read_tokens"] + group["cache_write_tokens"],
                output=group["output_tokens"],
                reasoning=group["reasoning_tokens"],
            )
        )
    lines.extend(["", "## Correctness/cost/speed Pareto frontier", ""])
    if summary["pareto_frontier"]:
        for item in summary["pareto_frontier"]:
            lines.append(
                f"- {item['model']} · {item['provider']} · {item['profile']} ({item['quality_metric']}-based)"
            )
    else:
        lines.append("No fully-accounted successful group yet.")
    lines.append("")
    return "\n".join(lines)


def write_report(
    directory: Path,
    manifest: dict[str, Any],
    records: list[dict[str, Any]],
    rescore: dict[str, Any] | None = None,
) -> dict[str, Any]:
    labels = read_labels(directory / "labels.jsonl")
    groups = aggregate(records, labels)
    catalog = load_snapshot(directory / "models.json")
    model_cards = []
    for candidate in manifest["configuration"]["candidates"]:
        model = catalog[candidate["model"]]
        model_cards.append(
            {
                "requested_model": candidate["model"],
                "requested_provider": candidate.get("provider"),
                "canonical_slug": model.get("canonical_slug"),
                "name": model.get("name"),
                "context_length": model.get("context_length"),
                "pricing": model.get("pricing"),
                "supported_parameters": model.get("supported_parameters"),
            }
        )
    funnel_fields = (
        "response_received",
        "response_contract_valid",
        "proposal_nonempty",
        "target_matched",
        "patch_accepted",
        "safety_passed",
        "build_passed",
        "oracle_matched",
    )
    scores = [record["score"] for record in records]
    funnel = {
        "attempts": len(scores),
        **{field: sum(bool(score.get(field)) for score in scores) for field in funnel_fields},
    }
    strata: dict[str, dict[str, int]] = {}
    for score in scores:
        tier = str(score.get("tier") or "unclassified")
        row = strata.setdefault(tier, {"attempts": 0, "build_passed": 0, "oracle_matched": 0})
        row["attempts"] += 1
        row["build_passed"] += bool(score.get("build_passed"))
        row["oracle_matched"] += bool(score.get("oracle_matched"))
    summary = {
        "schema": SUMMARY_SCHEMA,
        "campaign_schema": manifest["schema"],
        "models_sha256": manifest["models_sha256"],
        "configuration": manifest["configuration"],
        "repair_protocols": sorted(
            {
                str(record["score"].get("protocol") or "aitex-repair-protocol-v1")
                for record in records
            }
        ),
        "created_unix_ms": int(time.time() * 1000),
        "planned_jobs": len(manifest["jobs"]),
        "completed_jobs": len(records),
        "charged_microusd": sum(
            int(record["score"].get("cost_microusd") or 0) for record in records
        ),
        "budget_debit_microusd": sum(budget_debit(record) for record in records),
        "unknown_cost_records": sum(
            record["score"].get("cost_microusd") is None for record in records
        ),
        "reservation_overruns": sum(
            isinstance(record["score"].get("cost_microusd"), int)
            and record["score"]["cost_microusd"] > record["reservation_microusd"]
            for record in records
        ),
        "input_tokens": sum(
            int(record["score"].get("input_tokens") or 0)
            + int(record["score"].get("cache_read_tokens") or 0)
            + int(record["score"].get("cache_write_tokens") or 0)
            for record in records
        ),
        "output_tokens": sum(
            int(record["score"].get("output_tokens") or 0) for record in records
        ),
        "reasoning_tokens": sum(
            int(record["score"].get("reasoning_tokens") or 0) for record in records
        ),
        "groups": groups,
        "funnel": funnel,
        "tiers": strata,
        "pareto_frontier": pareto(groups),
        "model_cards": model_cards,
        "semantic_labels": sum(record["job_id"] in labels for record in records),
    }
    if rescore:
        summary["rescore"] = rescore
    atomic_write(directory / "summary.json", json.dumps(summary, indent=2, sort_keys=True).encode() + b"\n")
    atomic_write(directory / "report.md", markdown_report(summary).encode())
    return summary


def review_campaign(directory: Path, reviewer: str | None) -> None:
    manifest = read_json(directory / "campaign.json")
    if manifest.get("schema") != CAMPAIGN_SCHEMA:
        raise ValueError("campaign directory has an incompatible manifest")
    records = read_results(directory / "results.jsonl")
    labels = read_labels(directory / "labels.jsonl")
    remaining = [record for record in records if record["job_id"] not in labels]
    print(f"aiTeX semantic review · {len(remaining)} unlabeled / {len(records)} results")
    for record in remaining:
        score = record["score"]
        print("\n" + "=" * 72)
        # Model and provider identity stay hidden until after labeling so the
        # small pilot's semantic judgment is not a popularity contest.
        print(f"{score.get('case_id')} · {score.get('profile')} · blinded model")
        print(
            f"build={score.get('build_passed')} oracle={score.get('oracle_matched')} "
            f"safety={score.get('safety_passed')} cost=${int(score.get('cost_microusd') or 0) / 1_000_000:.6f}"
        )
        if score.get("repair_intent"):
            print(f"Intent: {score['repair_intent']}")
        if score.get("diagnostic_presentation"):
            print("Diagnostic:\n" + score["diagnostic_presentation"])
        if score.get("answer_cause"):
            print(f"Cause: {score['answer_cause']}")
        if score.get("patch"):
            print("Patch:\n" + score["patch"])
        elif score.get("raw_response"):
            print("Response:\n" + score["raw_response"])
        if score.get("error"):
            print(f"Error: {score['error']}")
        while True:
            answer = input("Label [c]orrect/[i]ncorrect/[u]ncertain/[s]kip/[q]uit: ").strip().lower()
            if answer in ("c", "i", "u", "s", "q"):
                break
        if answer == "q":
            break
        if answer == "s":
            continue
        note = input("Note (optional): ").strip()
        label = {"c": "correct", "i": "incorrect", "u": "uncertain"}[answer]
        value = {
            "schema": LABEL_SCHEMA,
            "job_id": record["job_id"],
            "label": label,
            "note": note,
            "reviewer": reviewer,
            "reviewed_unix_ms": int(time.time() * 1000),
        }
        append_jsonl(directory / "labels.jsonl", value)
        labels[record["job_id"]] = value
    summary = write_report(directory, manifest, records)
    print(
        f"review: {summary['semantic_labels']}/{len(records)} labeled · {directory / 'report.md'}"
    )


def export_qualification(directory: Path, output: Path) -> None:
    """Export aggregate campaign evidence without prompts, answers, or patches."""
    manifest_path = directory / "campaign.json"
    results_path = directory / "results.jsonl"
    models_path = directory / "models.json"
    labels_path = directory / "labels.jsonl"
    manifest = read_json(manifest_path)
    records = read_results(results_path)
    summary = write_report(directory, manifest, records)
    if summary["completed_jobs"] != summary["planned_jobs"]:
        raise ValueError("qualification export requires a complete campaign")
    expected_protocol = manifest.get("configuration", {}).get("repair_protocol")
    protocols = {
        record.get("score", {}).get("protocol")
        for record in records
    }
    if protocols != {expected_protocol}:
        raise ValueError(
            f"qualification export protocol mismatch: manifest {expected_protocol!r}, "
            f"scores {sorted(str(value) for value in protocols)}"
        )
    forbidden_failures = {
        "protocol-mismatch",
        "provider-output-limit-violation",
    }
    observed_forbidden = sorted(
        {
            str(record.get("failure_class"))
            for record in records
            if record.get("failure_class") in forbidden_failures
        }
    )
    if observed_forbidden:
        raise ValueError(
            "qualification export contains invalid experimental conditions: "
            + ", ".join(observed_forbidden)
        )
    if summary["unknown_cost_records"] or summary["reservation_overruns"]:
        raise ValueError("qualification export requires complete cost accounting without overruns")
    if any(not record.get("score", {}).get("served_provider") for record in records):
        raise ValueError("qualification export requires served-provider identity for every response")
    labels = read_labels(labels_path)
    if len(labels) != len(records):
        raise ValueError(
            f"qualification export requires semantic labels for every response: "
            f"{len(labels)}/{len(records)}"
        )
    configuration = manifest["configuration"]
    reviewers = sorted({str(label.get("reviewer") or "<unidentified>") for label in labels.values()})
    exported = {
        "schema": "aitex-model-campaign-qualification-v1",
        "campaign": {
            key: configuration.get(key)
            for key in (
                "repair_protocol",
                "track",
                "seed",
                "profiles",
                "repeats",
                "max_input_tokens",
                "max_output_tokens",
                "budget_microusd",
                "retries",
                "min_request_ms",
                "privacy",
                "candidates",
                "corpus_sha256",
                "lab_sha256",
                "aitex_sha256",
            )
        },
        "result": {
            key: summary[key]
            for key in (
                "planned_jobs",
                "completed_jobs",
                "semantic_labels",
                "charged_microusd",
                "budget_debit_microusd",
                "unknown_cost_records",
                "reservation_overruns",
                "input_tokens",
                "output_tokens",
                "reasoning_tokens",
                "groups",
                "pareto_frontier",
                "model_cards",
            )
        },
        "private_source_artifacts_sha256": {
            "campaign.json": file_digest(manifest_path),
            "results.jsonl": file_digest(results_path),
            "labels.jsonl": file_digest(labels_path),
            "models.json": file_digest(models_path),
        },
        "semantic_review": {
            "blinded_model_and_provider_identity": True,
            "reviewer_ids": reviewers,
        },
        "privacy": {
            "raw_prompts_responses_patches_published": False,
            "aggregate_contains_source_text": False,
        },
    }
    atomic_write(output, json.dumps(exported, indent=2, sort_keys=True).encode() + b"\n")
    print(f"export: aggregate qualification -> {output}")


def self_test() -> None:
    assert openrouter_config_path(
        {"XDG_CONFIG_HOME": "/tmp/config", "HOME": "/tmp/home"}
    ) == Path("/tmp/config/aitex/env")
    assert openrouter_config_path(
        {"XDG_CONFIG_HOME": "relative", "HOME": "/tmp/home"}
    ) == Path("/tmp/home/.config/aitex/env")
    assert (
        parse_openrouter_env(
            "# private credentials\nOTHER=value\nexport OPENROUTER_API_KEY='sk-or-test'\n"
        )
        == "sk-or-test"
    )
    try:
        parse_openrouter_env(
            "OPENROUTER_API_KEY=first\nOPENROUTER_API_KEY=second\n"
        )
        raise AssertionError("duplicate OpenRouter keys must fail")
    except ValueError:
        pass
    with tempfile.TemporaryDirectory(prefix="aitex-openrouter-config-") as raw_directory:
        credential = Path(raw_directory) / "env"
        atomic_write(credential, b"export OPENROUTER_API_KEY='sk-or-private'\n")
        assert read_openrouter_config(credential) == "sk-or-private"
        credential.chmod(0o644)
        try:
            read_openrouter_config(credential)
            raise AssertionError("public OpenRouter credential permissions must fail")
        except ValueError as error:
            assert "permissions 0644" in str(error)
            assert "sk-or-private" not in str(error)

    model = {"id": "vendor/model", "pricing": {"prompt": "0.000001", "completion": "0.000002", "request": "0"}}
    assert reserve_microusd(model, 100, 20) == 140
    assert budget_debit({"score": {"cost_microusd": 17}, "reservation_microusd": 140}) == 17
    assert budget_debit({"score": {"cost_microusd": None}, "reservation_microusd": 140}) == 140
    expensive_cache = {
        "id": "vendor/cached",
        "pricing": {
            "prompt": "0.000001",
            "completion": "0.000002",
            "request": "0",
            "input_cache_write": "0.000003",
            "internal_reasoning": "0.000004",
        },
    }
    assert reserve_microusd(expensive_cache, 100, 20) == 420
    candidates = parse_candidates(["vendor/model=provider/region"], "pinned", None)
    jobs = build_jobs(candidates, ["case-a", "case-b"], ["compact"], 2, 42)
    assert len(jobs) == 4 and len({job["job_id"] for job in jobs}) == 4
    assert jobs == build_jobs(candidates, ["case-a", "case-b"], ["compact"], 2, 42)
    rows = []
    for index, success in enumerate((True, False), 1):
        rows.append(
            {
                "schema": RESULT_SCHEMA,
                "job_id": str(index),
                "requested_provider": "provider-a",
                "reservation_microusd": 10,
                "failure_class": None if success else "compile-failure",
                "score": {
                    "protocol": REPAIR_PROTOCOL,
                    "requested_model": "vendor/model",
                    "served_provider": "provider-a",
                    "profile": "compact",
                    "build_passed": success,
                    "oracle_matched": success,
                    "safety_passed": True,
                    "cost_microusd": 10,
                    "input_tokens": 100,
                    "output_tokens": 20,
                    "reasoning_tokens": 5,
                    "model_ms": 50 + index,
                },
            }
        )
    labels = {
        "1": {"label": "correct"},
        "2": {"label": "incorrect"},
    }
    groups = aggregate(rows, labels)
    assert groups[0]["build_rate"] == 0.5
    assert groups[0]["charged_microusd"] == 20
    assert groups[0]["cost_per_attempt_microusd"] == 10
    assert groups[0]["cost_per_success_microusd"] == 20
    assert groups[0]["semantic_correct"] == 1
    assert groups[0]["semantic_rate_ci95"] is not None
    assert pareto(groups)
    original = {
        "requested_model": "vendor/model",
        "cost_microusd": 17,
        "model_ms": 51,
        "raw_response": "original",
        "build_passed": False,
        "error": "old evaluator error",
    }
    replayed = {
        "requested_model": "offline-replay",
        "cost_microusd": None,
        "model_ms": 0,
        "raw_response": "replacement",
        "build_passed": True,
        "error": None,
        "patch_normalization": "diff-marker-padding",
    }
    merged = merge_rescored_score(original, replayed)
    assert merged["requested_model"] == "vendor/model"
    assert merged["cost_microusd"] == 17 and merged["model_ms"] == 51
    assert merged["raw_response"] == "original"
    assert merged["build_passed"] is True and merged["error"] is None
    assert merged["patch_normalization"] == "diff-marker-padding"
    assert failure_class("api error 404: unavailable", None) == "routing-failure"
    assert failure_class("api error 403: denied", None) == "provider-refusal"
    assert failure_class("campaign protocol mismatch: expected v2", {}) == "protocol-mismatch"
    assert (
        failure_class(
            "model response: malformed",
            {"max_output_tokens": 512, "output_tokens": 8192},
        )
        == "provider-output-limit-violation"
    )
    assert failure_class("malformed unified-diff hunk header", None) == "invalid-patch"
    assert (
        failure_class(
            "isolated full build rejected the patch (Undefined control sequence.)",
            {"patch_accepted": True, "build_passed": False},
        )
        == "compile-failure"
    )
    insured = {"cost_microusd": None}
    account_zero_completion_insurance(insured, "routing-failure")
    assert insured == {
        "cost_microusd": 0,
        "cost_basis": "openrouter-zero-completion-insurance",
    }
    ambiguous = {"cost_microusd": None}
    account_zero_completion_insurance(ambiguous, "rate-limit")
    assert ambiguous == {"cost_microusd": None}
    with tempfile.TemporaryDirectory(prefix="aitex-model-campaign-") as raw_directory:
        directory = Path(raw_directory)
        snapshot = {"data": [{**model, "name": "Mock Model", "context_length": 4096}]}
        atomic_write(directory / "models.json", canonical(snapshot) + b"\n")
        corpus = directory / "corpus.json"
        atomic_write(corpus, canonical({"schema": "aitex-repair-corpus-v1"}) + b"\n")
        arguments = argparse.Namespace(
            corpus=corpus,
            profiles=["compact"],
            repeats=2,
            track="routed",
            seed=42,
            max_input_tokens=100,
            max_output_tokens=20,
            budget_microusd=1_000,
            retries=1,
            min_request_ms=0,
            aitex=Path(__file__).resolve(),
            lab=Path(__file__).resolve(),
            campaign=directory,
            snapshot=None,
            openrouter_ignore_provider=[],
        )
        campaign_candidates = parse_candidates(["vendor/model"], "routed", None)
        campaign_jobs = build_jobs(campaign_candidates, ["case-a"], ["compact"], 2, 42)
        manifest, snapshot_path = create_or_resume_campaign(
            arguments, campaign_candidates, campaign_jobs
        )
        resumed, resumed_snapshot = create_or_resume_campaign(
            arguments, campaign_candidates, campaign_jobs
        )
        assert resumed == manifest and resumed_snapshot == snapshot_path
        campaign_rows = json.loads(json.dumps(rows))
        for row, job in zip(campaign_rows, campaign_jobs):
            row["job_id"] = job["job_id"]
        for row in campaign_rows:
            append_jsonl(directory / "results.jsonl", row)
            append_jsonl(
                directory / "labels.jsonl",
                {
                    "schema": LABEL_SCHEMA,
                    "job_id": row["job_id"],
                    "label": "correct" if row["score"]["build_passed"] else "incorrect",
                    "note": "self-test",
                    "reviewer": "self-test",
                    "reviewed_unix_ms": 0,
                },
            )
        summary = write_report(directory, manifest, campaign_rows)
        assert summary["model_cards"][0]["name"] == "Mock Model"
        assert (directory / "summary.json").is_file()
        assert (directory / "report.md").is_file()
        assert (directory.stat().st_mode & 0o077) == 0
        qualification = directory / "qualification.json"
        export_qualification(directory, qualification)
        assert read_json(qualification)["result"]["semantic_labels"] == 2
    print("model campaign self-test: OK")


def parse_arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--model", action="append", default=[], help="MODEL or MODEL=PROVIDER; repeat for each candidate")
    parser.add_argument("--profiles", default="compact", help="comma-separated diagnostic,compact,expanded or all")
    parser.add_argument("--repeats", type=int, default=1)
    parser.add_argument("--track", choices=("routed", "pinned"), default="routed")
    parser.add_argument("--openrouter-provider", help="default provider slug for pinned candidates")
    parser.add_argument(
        "--openrouter-ignore-provider",
        action="append",
        default=[],
        help="provider slug to exclude; repeat as needed",
    )
    parser.add_argument("--budget-usd", required=False, help="conservative local campaign ceiling; live runs require it")
    parser.add_argument("--max-input-tokens", type=int, default=32_768, help="conservative per-request reservation ceiling")
    parser.add_argument("--max-output-tokens", type=int, default=1024)
    parser.add_argument("--seed", type=int, default=20260903)
    parser.add_argument("--retries", type=int, default=2, help="429 retries only")
    parser.add_argument("--backoff-seconds", type=float, default=1.0)
    parser.add_argument("--min-request-ms", type=int, default=0)
    parser.add_argument("--timeout", type=int, default=180)
    parser.add_argument("--campaign", type=Path)
    parser.add_argument("--snapshot", type=Path, help="use a supplied models JSON snapshot for a new campaign")
    parser.add_argument("--corpus", type=Path, default=Path("corpus/repair-lab/manifest.json"))
    parser.add_argument("--lab", type=Path, default=Path("target/debug/aitex-repair-lab"))
    parser.add_argument("--aitex", type=Path, default=Path("bin/aitex"))
    parser.add_argument("--plan-only", action="store_true")
    parser.add_argument("--report-only", action="store_true")
    parser.add_argument(
        "--export-qualification",
        type=Path,
        help="write a privacy-preserving aggregate only after full semantic review",
    )
    parser.add_argument(
        "--rescore",
        action="store_true",
        help="re-evaluate saved responses offline into an immutable content-addressed result tree",
    )
    parser.add_argument("--review", action="store_true", help="interactively label semantic correctness")
    parser.add_argument("--reviewer", help="pseudonymous reviewer ID stored with --review labels")
    parser.add_argument("--self-test", action="store_true")
    arguments = parser.parse_args()
    if arguments.self_test:
        return arguments
    campaign_was_supplied = arguments.campaign is not None
    if not arguments.campaign:
        stamp = time.strftime("%Y%m%d-%H%M%S", time.gmtime())
        arguments.campaign = Path(f".aitex/benchmarks/openrouter-{stamp}")
    profiles = [part for part in arguments.profiles.split(",") if part]
    arguments.profiles = list(PROFILE_INPUT_CEILINGS) if profiles == ["all"] else profiles
    if not arguments.profiles or any(profile not in PROFILE_INPUT_CEILINGS for profile in arguments.profiles):
        parser.error("--profiles must contain diagnostic, compact, expanded, or all")
    if arguments.repeats <= 0 or arguments.max_input_tokens <= 0 or arguments.max_output_tokens <= 0:
        parser.error("repeats and token ceilings must be positive")
    if arguments.retries < 0 or arguments.timeout <= 0 or arguments.min_request_ms < 0:
        parser.error("retry, timeout, and pacing values must be nonnegative")
    actions = sum(
        (
            arguments.report_only,
            arguments.review,
            arguments.rescore,
            arguments.export_qualification is not None,
        )
    )
    if actions > 1:
        parser.error(
            "--report-only, --review, --rescore, and --export-qualification are mutually exclusive"
        )
    if (
        arguments.report_only
        or arguments.review
        or arguments.rescore
        or arguments.export_qualification is not None
    ):
        if not campaign_was_supplied:
            parser.error(
                "--report-only, --review, --rescore, and --export-qualification require --campaign"
            )
        arguments.budget_microusd = 0
        return arguments
    if arguments.reviewer:
        parser.error("--reviewer requires --review")
    if not arguments.model:
        parser.error("at least one --model is required")
    if arguments.budget_usd is None:
        parser.error("campaigns require --budget-usd, including plan-only manifests")
    try:
        budget = Decimal(arguments.budget_usd or "0")
    except InvalidOperation:
        parser.error("--budget-usd must be a decimal amount")
    if budget < 0 or not budget.is_finite():
        parser.error("--budget-usd must be nonnegative")
    arguments.budget_microusd = int(
        (budget * Decimal(1_000_000)).to_integral_value(rounding=ROUND_CEILING)
    )
    return arguments


def main() -> int:
    arguments = parse_arguments()
    if arguments.self_test:
        self_test()
        return 0
    try:
        if arguments.report_only:
            manifest = read_json(arguments.campaign / "campaign.json")
            records = read_results(arguments.campaign / "results.jsonl")
            summary = write_report(arguments.campaign, manifest, records)
            print(f"report: {summary['completed_jobs']}/{summary['planned_jobs']} jobs · ${summary['charged_microusd'] / 1_000_000:.6f}")
            return 0
        if arguments.review:
            review_campaign(arguments.campaign, arguments.reviewer)
            return 0
        if arguments.rescore:
            directory, summary = rescore_campaign(arguments)
            print(
                f"rescore: {summary['completed_jobs']}/{summary['planned_jobs']} jobs · "
                f"no provider calls · {directory}"
            )
            return 0
        if arguments.export_qualification is not None:
            export_qualification(arguments.campaign, arguments.export_qualification)
            return 0

        candidates = parse_candidates(arguments.model, arguments.track, arguments.openrouter_provider)
        cases = model_cases(arguments.corpus)
        jobs = build_jobs(candidates, cases, arguments.profiles, arguments.repeats, arguments.seed)
        if not arguments.lab.is_file():
            raise ValueError(
                f"repair-lab binary is missing: {arguments.lab}; run make repair-lab"
            )
        if not arguments.aitex.is_file():
            raise ValueError(f"aiTeX binary is missing: {arguments.aitex}; run make")
        manifest, snapshot_path = create_or_resume_campaign(arguments, candidates, jobs)
        models = load_snapshot(snapshot_path)
        if manifest["jobs"] != jobs:
            raise ValueError("campaign job matrix changed")
        for candidate in candidates:
            if candidate.model not in models:
                raise ValueError(f"model is absent from frozen snapshot: {candidate.model}")
        total_reservation = sum(
            reserve_microusd(
                models[job["model"]],
                reservation_input_tokens(arguments, job),
                arguments.max_output_tokens,
            )
            for job in jobs
        )
        print(f"aiTeX model campaign · {len(candidates)} models · {len(jobs)} jobs · {arguments.track}")
        print(f"  Catalog   {snapshot_path}")
        print(f"  Ceiling   ${arguments.budget_microusd / 1_000_000:.6f} local · ${total_reservation / 1_000_000:.6f} requested-limit reservations")
        if arguments.plan_only:
            summary = write_report(arguments.campaign, manifest, read_results(arguments.campaign / "results.jsonl"))
            print(f"  Plan      {arguments.campaign} · {summary['completed_jobs']} already complete")
            return 0
        if not load_openrouter_api_key():
            path = openrouter_config_path()
            suffix = f" or {path}" if path else ""
            raise ValueError(
                f"OPENROUTER_API_KEY is required for live jobs; set it in the environment{suffix}"
            )

        result_path = arguments.campaign / "results.jsonl"
        records = read_results(result_path)
        completed = {record["job_id"] for record in records}
        spent = sum(budget_debit(record) for record in records)
        last_request = 0.0
        for job in jobs:
            if job["job_id"] in completed:
                continue
            reservation = reserve_microusd(
                models[job["model"]],
                reservation_input_tokens(arguments, job),
                arguments.max_output_tokens,
            )
            if spent + reservation > arguments.budget_microusd:
                print(f"  Budget    stopped safely at ${spent / 1_000_000:.6f}; next reservation ${reservation / 1_000_000:.6f}")
                break
            wait = arguments.min_request_ms / 1000 - (time.monotonic() - last_request)
            if wait > 0:
                time.sleep(wait)
            print(f"  Job       {job['order']}/{len(jobs)} · {job['model']} · {job['case']} · {job['profile']}", flush=True)
            record = run_job(arguments, job, models[job["model"]], arguments.campaign, reservation)
            last_request = time.monotonic()
            append_jsonl(result_path, record)
            records.append(record)
            cost = record["score"].get("cost_microusd")
            outcome = "build" if record["score"].get("build_passed") else record.get("failure_class")
            cost_label = (
                f"unknown (reserved ${reservation / 1_000_000:.6f})"
                if cost is None
                else f"${cost / 1_000_000:.6f}"
            )
            print(f"             {outcome} · {record['score'].get('model_ms', 0):.0f} ms · {cost_label}")
            spent += budget_debit(record)
            if record.get("failure_class") == "protocol-mismatch":
                print("  Safety    stopped: repair-lab protocol differs from the frozen campaign")
                break
            if isinstance(cost, int) and cost > reservation:
                print(
                    f"  Safety    stopped: authoritative charge ${cost / 1_000_000:.6f} "
                    f"exceeded the ${reservation / 1_000_000:.6f} reservation"
                )
                break
        summary = write_report(arguments.campaign, manifest, records)
        print(
            f"  Results   {summary['completed_jobs']}/{summary['planned_jobs']} · "
            f"${summary['charged_microusd'] / 1_000_000:.6f} charged · "
            f"${summary['budget_debit_microusd'] / 1_000_000:.6f} budget debit"
        )
        print(f"  Report    {arguments.campaign / 'report.md'}")
        return 0
    except (OSError, ValueError, json.JSONDecodeError) as error:
        print(f"model campaign: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
