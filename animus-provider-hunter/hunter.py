#!/usr/bin/env python3
"""
animus-provider-hunter/hunter.py

Discovers free-tier LLM API providers. Outputs JSON array of ProviderCandidate dicts.
Called by Animus via shell_exec: python3 /path/to/hunter.py
"""
import json
import sys
import httpx
from bs4 import BeautifulSoup


def discover_openrouter() -> list[dict]:
    """Scrape OpenRouter for models with $0 pricing."""
    candidates = []
    try:
        resp = httpx.get(
            "https://openrouter.ai/api/v1/models",
            timeout=15,
            headers={"User-Agent": "Mozilla/5.0 (compatible; AnimusProviderHunter/1.0)"}
        )
        if resp.status_code != 200:
            return []
        data = resp.json()
        for model in data.get("data", []):
            pricing = model.get("pricing", {})
            prompt_price = float(pricing.get("prompt", "1"))
            if prompt_price == 0.0:
                provider_id = model["id"].split("/")[0].lower() if "/" in model["id"] else "openrouter"
                candidates.append({
                    "name": model.get("name", model["id"]),
                    "provider_id": provider_id,
                    "model_id": model["id"],
                    "signup_url": "https://openrouter.ai",
                    "api_docs_url": "https://openrouter.ai/docs",
                    "free_tier_desc": "Free tier via OpenRouter aggregator",
                    "base_url": "https://openrouter.ai/api/v1",
                    "hq_country_hint": "US",
                })
    except Exception as e:
        print(f"[hunter] openrouter scrape failed: {e}", file=sys.stderr)
    return candidates


def discover_groq() -> list[dict]:
    """Groq has a free tier. Return as a known candidate."""
    return [{
        "name": "Groq",
        "provider_id": "groq",
        "model_id": "llama-3.1-8b-instant",
        "signup_url": "https://console.groq.com/keys",
        "api_docs_url": "https://console.groq.com/docs/openai",
        "free_tier_desc": "Free tier with daily rate limits",
        "base_url": "https://api.groq.com/openai/v1",
        "hq_country_hint": "US",
    }]


def discover() -> list[dict]:
    candidates = []
    candidates.extend(discover_openrouter())
    candidates.extend(discover_groq())
    # Deduplicate by provider_id
    seen = set()
    unique = []
    for c in candidates:
        key = c["provider_id"]
        if key not in seen:
            seen.add(key)
            unique.append(c)
    return unique


if __name__ == "__main__":
    results = discover()
    print(json.dumps(results, indent=2))
