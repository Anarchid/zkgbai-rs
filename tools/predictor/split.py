"""Train/test split by game id (predeclared): hash mod 5 == 0 -> test; human games separate.

Live human logs (~/.spring/combatlog) are "human"; human demos re-run through the widget
(predictor/replays, tools/arena/replay-harvest.sh) are "replay". Neither is trained on.
PRED_DEV_FOLD=k carves a development fold out of train (md5 mod 5 == k, k in 1..4): "dev", which
no fitter trains on (mechanics round 1)."""
import hashlib
import os

DEV_FOLD = int(os.environ.get("PRED_DEV_FOLD", "0"))


def is_human(series):
    return "__" not in series  # human logs live in ~/.spring/combatlog, not under zkgbai-runs


def is_replay(series):
    return series.startswith("predictor__replays__")


def fold(game_id):
    return int(hashlib.md5(game_id.encode()).hexdigest(), 16) % 5


def split_of(game_id, series):
    if is_human(series):
        return "human"
    if is_replay(series):
        return "replay"
    k = fold(game_id)
    if k == 0:
        return "test"
    return "dev" if DEV_FOLD and k == DEV_FOLD else "train"
