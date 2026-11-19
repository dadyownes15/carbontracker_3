# Server example

A FastAPI server where every request is a span. Requests that run at the same
time share the energy used while they overlap.

| File | Does |
|---|---|
| `app.py` | The server: `/chat` and `/embed`, each a span |
| `simulate.py` | Stand-ins for a language model and an encoder |
| `client.py` | Sends the requests, several at a time |

Install the extras once:

```sh
pip install -r requirements.txt
```

Start the server in one terminal:

```sh
uvicorn app:app
```

Send load from another. The defaults are 40 chat and 200 embed requests, 20 at a time:

```sh
python client.py
```

Stop the server with Ctrl-C, which finishes the run, then read the record:

```sh
carbontracker report --spans logs
```

```
Span   Count   Total     Mean       Min      Max       CO2eq
embed    200  43.4 J  0.217 J  0.0694 J  0.420 J  0.00173 g
chat      40  30.7 J  0.766 J   0.287 J   2.74 J  0.00122 g
```

The server does not return a measurement per request. A span has no result of
its own, since concurrent requests share energy; the report gives the totals per
span name. From Python: `report("logs").spans`.
