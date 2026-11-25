"""Run the pinned HTTP integration tests against both server processes."""
import argparse
import asyncio
from contextlib import contextmanager
from http.client import HTTPConnection
import os
from pathlib import Path
import runpy
import socket
import subprocess
import sys
import tempfile
import time

from differential import verify_source


@contextmanager
def running(cli, root, env, flags):
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    with tempfile.TemporaryFile() as log:
        proc = subprocess.Popen([*cli, "serve", "--http", "--port", str(port), *flags],
                                cwd=root, env=env, stdin=subprocess.DEVNULL,
                                stdout=log, stderr=log)
        try:
            deadline = time.monotonic() + 30
            while proc.poll() is None and time.monotonic() < deadline:
                try:
                    with socket.create_connection(("127.0.0.1", port), timeout=.2):
                        break
                except OSError:
                    time.sleep(.1)
            else:
                log.seek(0)
                raise AssertionError(log.read().decode(errors="replace"))
            yield port
        finally:
            proc.terminate()
            try:
                proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait(timeout=10)


def header_cases(port):
    results = []
    for host, origin, denied in [
        ("localhost", None, False), ("127.1.2.3", None, False),
        (f"[::1]:{port}", f"http://localhost:{port}", False),
        ("example.invalid", None, True), ("localhost:1", None, True),
        ("127.0.0.1", "https://example.invalid", True),
        ("localhost", "http://localhost:1", True), ("localhost", "null", True),
    ]:
        client = HTTPConnection("127.0.0.1", port, timeout=3)
        try:
            headers = {"Host": host, **({"Origin": origin} if origin is not None else {})}
            client.request("GET", "/mcp", headers=headers)
            response = client.getresponse()
            assert (response.status == 403) == denied, (host, origin, response.status)
            results.append(response.read().decode() if denied else "accepted")
        finally:
            client.close()
    return results


async def queries(tests, info, cli, env):
    for name in [
        "test_each_call_answers_from_the_repo_it_names",
        "test_a_call_naming_no_repo_or_an_unregistered_one_is_refused",
        "test_the_handshake_tells_the_client_to_name_a_repo",
    ]:
        await tests[name](info)
    path = str(info["a"])
    async with tests["streamable_http_client"](info["url"]) as streams:
        async with tests["ClientSession"](*streams[:2]) as session:
            handshake = await session.initialize()
            listing = await session.list_tools()
            tool = "list_graph_stats_tool"
            cases = []
            for value in ({}, {"repo_root": "relative"}, {"repo_root": str(info["stranger"])}):
                resp = await session.call_tool(tool, value)
                assert tests["_is_error"](resp)
                cases.append(tests["_text"](resp))
            subprocess.run([*cli, "unregister", path], check=True, cwd=info["a"], env=env, capture_output=True)
            resp = await session.call_tool(tool, {"repo_root": path})
            assert tests["_is_error"](resp)
            cases.append(tests["_text"](resp))
            return handshake.instructions, listing.model_dump(by_alias=True), cases


async def single(tests, port):
    async with tests["streamable_http_client"](f"http://127.0.0.1:{port}/mcp") as streams:
        async with tests["ClientSession"](*streams[:2]) as session:
            handshake = await session.initialize()
            listing = await session.list_tools()
            result = await session.call_tool("get_docs_section_tool", {"section_name": "watch"})
            return handshake.instructions, listing.model_dump(by_alias=True), tests["_payload"](result)


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--upstream", required=True, type=Path)
    p.add_argument("--binary", required=True, type=Path)
    a = p.parse_args()
    upstream, native = a.upstream.resolve(), a.binary.resolve()
    verify_source(upstream)
    sys.path.insert(0, str(upstream))
    tests = runpy.run_path(str(upstream/"tests/test_http_multi_repo.py"))
    base_env = {k: v for k, v in os.environ.items() if not k.startswith(("CRG_", "PYTHON"))}
    observed = []
    with tempfile.TemporaryDirectory(prefix="crg-http-") as temp:
        base = Path(temp).resolve()
        for folder, symbol in (("repo_a", "only_in_a"), ("repo_b", "only_in_b"), ("stranger", "only_in_stranger")):
            tests["_make_repo"](base, folder, symbol)
        for index, cli in enumerate(([sys.executable, "-m", "code_review_graph"], [str(native)])):
            env = dict(base_env, CRG_HOME=str(base/f"registry-{index}"), PYTHONPATH=str(upstream),
                       PYTHONDONTWRITEBYTECODE="1", CRG_PARSE_WORKERS="1", NO_COLOR="1")
            for name in ("repo_a", "repo_b"):
                subprocess.run([*cli, "register", str(base/name)], env=env, check=True, capture_output=True)
            with running(cli, base/"repo_a", env, ["--multi-repo"]) as port:
                info = dict(url=f"http://127.0.0.1:{port}/mcp", a=base/"repo_a", b=base/"repo_b",
                            stranger=base/"stranger")
                multi = asyncio.run(asyncio.wait_for(queries(tests, info, cli, env), 60))
                guard = header_cases(port)
            with running(cli, base/"repo_a", env, ["--repo", str(base/"repo_b"), "--tools", "get_docs_section_tool"]) as port:
                scoped = asyncio.run(asyncio.wait_for(single(tests, port), 15))
            errors = []
            for flags in (["--port", "5554"], ["--host", "localhost"], ["--multi-repo"],
                          ["--http", "--multi-repo", "--repo", str(base/"repo_a")],
                          ["--http", "--multi-repo", "--auto-watch"]):
                proc = subprocess.run([*cli, "serve", *flags], env=env, capture_output=True, timeout=5)
                assert proc.returncode == 2
                errors.append((proc.stdout, proc.stderr))
            observed.append((multi, guard, scoped, errors))
        for i, name in enumerate(("multi-repo", "header handling", "single-repo", "CLI validation")):
            assert observed[0][i] == observed[1][i], name
    print("HTTP parity: concurrent registered repositories, live unregister, handshake, tool catalog, scoped single-repo sessions, eight Host/Origin checks and five CLI errors matched")


if __name__ == "__main__":
    main()
