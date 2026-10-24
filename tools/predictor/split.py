"""Train/test split by game id (predeclared): hash mod 5 == 0 -> test; human games separate."""
import hashlib


def is_human(series):
    return "__" not in series  # human logs live in ~/.spring/combatlog, not under zkgbai-runs


def fold(game_id):
    return int(hashlib.md5(game_id.encode()).hexdigest(), 16) % 5


def split_of(game_id, series):
    if is_human(series):
        return "human"
    return "test" if fold(game_id) == 0 else "train"
