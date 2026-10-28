"""Train/test split by game id (predeclared): hash mod 5 == 0 -> test; human games separate.

Live human logs (~/.spring/combatlog) are "human"; human demos re-run through the widget
(predictor/replays, tools/arena/replay-harvest.sh) are "replay". Neither is trained on."""
import hashlib


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
    return "test" if fold(game_id) == 0 else "train"
