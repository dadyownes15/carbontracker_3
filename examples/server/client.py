"""Send chat and embed requests to the server, several at a time.

    python client.py                       # 40 chat and 200 embed requests
    python client.py --chat 10 --embed 50 --concurrency 5
"""

import argparse
import asyncio

import httpx


async def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--url", default="http://127.0.0.1:8000")
    parser.add_argument("--chat", type=int, default=40)
    parser.add_argument("--embed", type=int, default=200)
    parser.add_argument("--concurrency", type=int, default=20)
    args = parser.parse_args()

    gate = asyncio.Semaphore(args.concurrency)
    async with httpx.AsyncClient(base_url=args.url, timeout=60) as client:

        async def send(path, **params):
            async with gate:
                response = await client.post(path, params=params)
                response.raise_for_status()

        await asyncio.gather(
            *[send("/chat", prompt=f"hello {i}") for i in range(args.chat)],
            *[send("/embed", text=f"document {i}") for i in range(args.embed)],
        )
    print(f"sent {args.chat} chat and {args.embed} embed requests")


if __name__ == "__main__":
    asyncio.run(main())
