"""The ``carbontracker`` command: ``carbontracker python train.py``.

The CLI itself is Rust (the ``carbontracker-cli`` crate); this runs it.
"""

import signal
import sys

from . import _native


def main() -> None:
    # SIGINT as for the native binary: the CLI handles it while the command
    # runs, so Python raises no KeyboardInterrupt.
    signal.signal(signal.SIGINT, signal.SIG_DFL)
    sys.exit(_native.cli_main(sys.argv))


if __name__ == "__main__":
    main()
