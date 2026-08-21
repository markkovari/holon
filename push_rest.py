import subprocess
for app in ["budget", "power"]:
    branch = f"feature/{app}-app-17873385{'07' if app == 'budget' else '15'}"
    subprocess.run(["git", "checkout", branch], check=True)
    subprocess.run(["git", "checkout", "showcase/graph-visualizer", "--", f"components/{app}-domain", f"examples/{app}"], check=True)
    subprocess.run(["git", "add", "."], check=True)
    res = subprocess.run(["git", "commit", "-m", f"Upgrade {app} to fully fledged SPA with auth and KV"], check=False)
    if res.returncode == 0:
        subprocess.run(["git", "push", "origin", branch], check=True)
