"""Minimal WebDriver client for geckodriver and headless Firefox (stdlib only)."""
import base64, json, subprocess, sys, time, urllib.request

class WD:
    def __init__(self, port=4444, width=390, height=844, dark=False):
        self.base = f"http://127.0.0.1:{port}"
        self.proc = subprocess.Popen(["geckodriver", "--port", str(port)], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        for _ in range(100):
            try:
                self._req("GET", "/status"); break
            except Exception:
                time.sleep(0.1)
        prefs = {"ui.systemUsesDarkTheme": 1 if dark else 0}
        caps = {"capabilities": {"alwaysMatch": {"moz:firefoxOptions": {"args": ["-headless"], "prefs": prefs}}}}
        self.sid = self._req("POST", "/session", caps)["value"]["sessionId"]
        # Headless Firefox keeps windows at least 500 CSS px wide; a phone
        # viewport is an iframe of the phone's size (see open_phone).
        self.cmd("POST", "window/rect", {"width": max(width, 500) + 40, "height": height + 120})
        self.size = (width, height)
        self.frame = None

    def _req(self, method, path, body=None):
        data = None if body is None else json.dumps(body).encode()
        r = urllib.request.Request(self.base + path, data=data, method=method, headers={"Content-Type": "application/json"})
        with urllib.request.urlopen(r, timeout=60) as resp:
            return json.loads(resp.read())

    def cmd(self, method, path, body=None):
        return self._req(method, f"/session/{self.sid}/{path}", body)["value"]

    def go(self, url):
        self.cmd("POST", "url", {"url": url})

    def js(self, script, *args):
        return self.cmd("POST", "execute/sync", {"script": script, "args": list(args)})

    def open_phone(self, url):
        """Loads `url` in a phone-sized iframe and switches into it: the page
        lays out and runs its media queries at the phone's width."""
        self.cmd("POST", "frame", {"id": None})
        self.go("about:blank")
        w, h = self.size
        self.js("document.body.style.margin = '8px';"
                "const f = document.createElement('iframe');"
                f"f.style.cssText = 'border:1px solid #888;width:{w}px;height:{h}px';"
                "f.src = arguments[0]; document.body.appendChild(f);", url)
        self.frame = self.cmd("POST", "element", {"using": "css selector", "value": "iframe"})
        self.cmd("POST", "frame", {"id": self.frame})
        for _ in range(100):
            if self.js("return document.readyState") == "complete" and self.js("return location.href") != "about:blank":
                break
            time.sleep(0.1)
        time.sleep(0.5)

    def shot_phone(self, path):
        """A screenshot of the phone iframe."""
        self.cmd("POST", "frame/parent", {})
        ref = next(iter(self.frame.values()))
        png = self.cmd("GET", f"element/{ref}/screenshot")
        open(path, "wb").write(base64.b64decode(png))
        self.cmd("POST", "frame", {"id": self.frame})

    def shot(self, path, full=False):
        png = self.cmd("GET", "moz/screenshot/full" if full else "screenshot")
        open(path, "wb").write(base64.b64decode(png))

    def quit(self):
        try:
            self._req("DELETE", f"/session/{self.sid}")
        finally:
            self.proc.terminate()
