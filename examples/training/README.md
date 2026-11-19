# Training example

Three small scripts that track a fake training run. The work is a busy loop, so
each runs anywhere in a few seconds. Run them from this folder.

| Script | Shows | README section |
|---|---|---|
| `train.py` | The simplest: `@track` on the function that does the work | Track one function |
| `train_profiling.py` | `train` and `eval` spans in each epoch, `span.mark`, and the span report | Track a training loop, See which part costs what |
| `train_budget.py` | Stopping once `predict()` passes a budget | Stop before a run gets too expensive |

```sh
python train.py
python train_profiling.py
python train_budget.py
```

Each run leaves a folder in `logs/`, which git ignores. Read them all with:

```sh
carbontracker report --spans logs
```

To track a script without touching its code:

```sh
carbontracker --country DK python train.py
```
