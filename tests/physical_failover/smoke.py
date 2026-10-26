#!/usr/bin/env python3
"""Disposable Patroni elections, index failover, and former-primary rejoining.

Requires Docker Compose and PARADEDB_HA_IMAGE built with target patroni-test.
Creates a unique Compose project, exposes no host ports, and removes only that
project's containers and volumes. Never accepts production database connections.
"""

import os
from pathlib import Path
import subprocess
import time
import uuid


HERE = Path(__file__).resolve().parent
PROJECT = f"pg-search-ha-test-{uuid.uuid4().hex[:10]}"
COMPOSE = ["docker", "compose", "--project-name", PROJECT, "--file", str(HERE / "compose.yml")]
NODES = ("node1", "node2")


def compose(*args, check=True):
    return subprocess.run(
        [*COMPOSE, *args], text=True, capture_output=True, check=check, timeout=120
    )


def sql(node, statement, check=True):
    return compose(
        "exec", "-T", node, "psql", "-U", "postgres", "-d", "postgres",
        "-At", "-v", "ON_ERROR_STOP=1", "-v", "VERBOSITY=verbose", "-c", statement,
        check=check,
    )


def wait_for(description, predicate, timeout=120):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        result = predicate()
        if result:
            return result
        time.sleep(1)
    raise AssertionError(f"Timed out waiting for {description}")


def primary():
    for node in NODES:
        # Use Patroni's primary health endpoint, not only the PostgreSQL recovery
        # flag: routing must agree with the DCS leader lease.
        result = compose(
            "exec", "-T", node, "python3", "-c",
            "import urllib.request; urllib.request.urlopen('http://127.0.0.1:8008/primary', timeout=2)",
            check=False,
        )
        if result.returncode == 0:
            return node
    return None


def wait_for_replay(leader, replica):
    lsn = sql(leader, "SELECT pg_current_wal_flush_lsn()::text").stdout.strip()
    # The LSN comes from PostgreSQL, not an external input.
    def caught_up():
        result = sql(
            replica,
            f"SELECT pg_is_in_recovery() AND COALESCE(pg_last_wal_replay_lsn() >= '{lsn}'::pg_lsn, false)",
            check=False,
        )
        return result.returncode == 0 and result.stdout.strip() == "t"
    wait_for(f"{replica} to replay {lsn}", caught_up)


def search_rows(node):
    return sql(node, "SELECT id, body FROM items WHERE body ||| 'shoes' ORDER BY id").stdout


def verify(node):
    result = sql(node, "SELECT bool_and(passed) FROM pdb.verify_index('items_idx', heapallindexed => true)")
    assert result.stdout.strip() == "t", result.stdout


def main():
    if not os.environ.get("PARADEDB_HA_IMAGE"):
        raise SystemExit("Set PARADEDB_HA_IMAGE to the locally built patroni-test image")
    succeeded = False
    try:
        compose("up", "--detach")
        leader = wait_for("initial Patroni primary", primary)
        replica = next(node for node in NODES if node != leader)
        wait_for_replay(leader, replica)
        print(f"Initial primary: {leader}; replica: {replica}", flush=True)
        sql(leader, "CREATE EXTENSION pg_search CASCADE")
        sql(leader, "CREATE TABLE items (id bigint PRIMARY KEY, body text)")
        sql(leader, "INSERT INTO items SELECT i, 'replicated shoes ' || i FROM generate_series(1, 1000) s(i)")
        sql(leader, "CREATE INDEX items_idx ON items USING paradedb (id, body)")
        sql(leader, "DELETE FROM items WHERE id % 7 = 0")
        sql(leader, "UPDATE items SET body = 'updated headphones' WHERE id = 3")
        sql(leader, "VACUUM items")
        wait_for_replay(leader, replica)

        denied = sql(replica, "SELECT id FROM items WHERE body ||| 'shoes'", check=False)
        assert denied.returncode != 0, "Standby search unexpectedly succeeded"
        assert "0A000" in denied.stderr and "cannot be read during recovery" in denied.stderr, denied.stderr
        # A rejected search must leave ordinary replication and queries healthy.
        sql(leader, "INSERT INTO items VALUES (10001, 'after rejected standby search shoes')")
        wait_for_replay(leader, replica)
        assert sql(replica, "SELECT count(*) FROM items WHERE id = 10001").stdout.strip() == "1"

        # Two real Patroni elections, with rejoining and streaming in between.
        # Never REINDEX on a promoted member: exercise the copied index itself.
        for cycle in range(2):
            expected = search_rows(leader)
            wait_for_replay(leader, replica)
            compose("kill", "--signal", "KILL", leader)
            promoted = wait_for(f"Patroni election of {replica}", primary)
            assert promoted == replica, (promoted, replica)
            assert search_rows(promoted) == expected, f"Copied index mismatch after election {cycle + 1}"
            assert sql(promoted, "SELECT pg_is_in_recovery()").stdout.strip() == "f"
            sql(promoted, f"INSERT INTO items VALUES ({11000 + cycle}, 'promoted shoes {cycle}')")
            sql(promoted, "VACUUM items")
            verify(promoted)
            print(f"Election {cycle + 1}: {promoted} serves copied index and accepts indexed writes", flush=True)

            compose("start", leader)
            wait_for_replay(promoted, leader)
            print(f"Former primary {leader} rejoined and replayed new index WAL", flush=True)
            leader, replica = promoted, leader
        succeeded = True
        print("Patroni physical-index failover smoke test passed", flush=True)
    finally:
        logs = compose("logs", "--no-color", check=False)
        if not succeeded:
            print(logs.stdout, flush=True)
            print(logs.stderr, flush=True)
        compose("down", "--volumes", "--remove-orphans", check=False)


if __name__ == "__main__":
    main()
