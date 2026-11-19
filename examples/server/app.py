"""A FastAPI server where every request is a carbontracker span.

    uvicorn app:app

Stop the server with Ctrl-C to finish the run, then:

    carbontracker report --spans logs
"""

from contextlib import asynccontextmanager

from fastapi import FastAPI

import simulate
from carbontracker import CarbonTracker

tracker = CarbonTracker(log_dir="logs")


@asynccontextmanager
async def lifespan(_app):
    yield
    tracker.stop()


app = FastAPI(lifespan=lifespan)


@app.post("/chat")
@tracker.span("chat")
async def chat(prompt: str):
    return await simulate.generate(prompt)


@app.post("/embed")
@tracker.span("embed")
async def embed(text: str):
    return await simulate.embed(text)
