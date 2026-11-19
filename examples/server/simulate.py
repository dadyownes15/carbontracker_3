"""Stand-ins for a language model and an encoder: a busy loop for a while."""

import asyncio
import time


def _busy(seconds):
    end = time.perf_counter() + seconds
    while time.perf_counter() < end:
        pass


async def generate(prompt):
    await asyncio.to_thread(_busy, 0.20)
    return f"echo: {prompt}"


async def embed(text):
    await asyncio.to_thread(_busy, 0.02)
    return [float(len(text)), 0.0, 1.0]
