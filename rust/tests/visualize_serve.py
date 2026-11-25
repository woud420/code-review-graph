"""Exercise the native and reference visualization HTTP servers against the same graph."""

import http.client
import signal
import socket
import subprocess
import time


def snapshot(command, repo, env, data):
    # Both published CLIs use the same fixed port. Never probe an unrelated server.
    with socket.socket() as reservation:
        reservation.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        reservation.bind(("127.0.0.1", 8765))
    child = subprocess.Popen(
        [*command, "visualize", "--serve", "--sidecar", "--seed-symbol", "read"],
        cwd=repo, env=env | {"PYTHONUNBUFFERED": "1"}, stdout=subprocess.PIPE,
        stderr=subprocess.PIPE, text=True, encoding="utf-8",
    )
    try:
        for _ in range(200):
            if child.poll() is not None:
                raise AssertionError(child.communicate())
            try:
                with socket.create_connection(("127.0.0.1", 8765), timeout=0.5):
                    break
            except ConnectionRefusedError:
                time.sleep(0.025)
        else:
            raise AssertionError("visualize --serve did not start")
        responses = {}
        for method, path in (("GET", "/"), ("GET", "/graph.html"), ("HEAD", "/graph.html"),
                             ("GET", "/graph.data.js"), ("GET", "/d3.v7.min.js"),
                             ("GET", "/subdir/index.html"), ("GET", "/no-such-file")):
            conn = http.client.HTTPConnection("127.0.0.1", 8765, timeout=5)
            try:
                conn.request(method, path)
                reply = conn.getresponse()
                body = reply.read()
                if path == "/no-such-file":
                    assert reply.status == 404
                    responses[(method, path)] = reply.status
                else:
                    assert reply.status == 200, (path, reply.status)
                    if method == "GET" and path != "/":
                        assert body == (data / path[1:]).read_bytes(), path
                    if path == "/":
                        assert b'<a href="graph.html">graph.html</a>' in body
                    if method == "HEAD":
                        assert not body
                    responses[(method, path)] = reply.getheader("Content-Type"), body
            finally:
                conn.close()
        child.send_signal(signal.SIGINT)
        out, err = child.communicate(timeout=8)
        assert child.returncode == 0 and "Server stopped." in out, (child.returncode, out, err)
        return responses, out
    finally:
        if child.poll() is None:
            child.terminate()
            child.wait(timeout=5)


def compare(repo, data, python, native, pyenv, env):
    subdir = data / "subdir"
    subdir.mkdir()
    (subdir / "index.html").write_text("<p>static local asset</p>\n")
    expected = snapshot(python, repo, pyenv, data)
    actual = snapshot(native, repo, env, data)
    assert expected[0].keys() == actual[0].keys()
    for key, value in expected[0].items():
        assert value == actual[0][key], key
    assert expected[1] == actual[1], (expected[1], actual[1])
    print("Live visualization servers agree on graph assets, directory listing, HEAD, 404 and SIGINT")
