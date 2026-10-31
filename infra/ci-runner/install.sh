#!/usr/bin/env bash
# Self-hosted runners for ci.yml's heavy jobs, as containers on this Mac (OrbStack).
#
#   ./install.sh install     build the image and (re)start the runners
#   ./install.sh uninstall   stop them and remove their registration (keeps the volumes)
#   ./install.sh status      what GitHub sees
#   ./install.sh clean       empty the target/ volume every runner builds in (holon-ci-target)
#
# The jobs pick them only while the repo variable CI_RUNNER is `picur`, and never for a PR from a
# fork (ci.yml). `gh variable delete CI_RUNNER` sends every job back to GitHub's runners.
# Containers, not the Mac itself: a job does not see this machine's home directory. It can still
# reach the Mac's loopback services through OrbStack, so only this repo's own branches run here.
set -euo pipefail
cd "$(dirname "$0")"
REPO=markkovari/holon
RUNNERS=(picur-1 picur-2 picur-3)
IMAGE=holon-ci-runner

case "${1:-}" in
  install)
    docker build -t "$IMAGE" .
    RHOME="$(docker run --rm --entrypoint printenv "$IMAGE" HOME)"
    for n in "${RUNNERS[@]}"; do
      # Killing a runner mid-job orphans the job, and its session blocks the next container for minutes.
      busy="$(gh api "repos/$REPO/actions/runners" -q ".runners[] | select(.name == \"$n\") | .busy")"
      [ "$busy" = true ] && { echo "$n is running a job; try again when it is idle"; exit 1; }
      docker stop -t 60 "$n" >/dev/null 2>&1 || true
      docker rm "$n" >/dev/null 2>&1 || true
      token=""
      # A registration lives on the volume; only a new volume needs a token.
      if ! docker run --rm -v "$n:$RHOME" --entrypoint test "$IMAGE" -f "$RHOME/.registration/.runner"; then
        token="$(gh api -X POST "repos/$REPO/actions/runners/registration-token" -q .token)"
      fi
      docker run -d --name "$n" --restart unless-stopped --cpus 6 --memory 6g \
        -v "$n:$RHOME" -v holon-ci-target:/ci-target -e REPO_URL="https://github.com/$REPO" -e RUNNER_NAME="$n" \
        -e REG_TOKEN="$token" "$IMAGE" >/dev/null
      echo "started $n"
    done
    gh variable set CI_RUNNER --repo "$REPO" --body picur
    echo "CI_RUNNER=picur: the heavy jobs now run here" ;;
  uninstall)
    RHOME="$(docker run --rm --entrypoint printenv "$IMAGE" HOME)"
    gh variable delete CI_RUNNER --repo "$REPO" 2>/dev/null || true
    for n in "${RUNNERS[@]}"; do
      docker rm -f "$n" >/dev/null 2>&1 || true
      id="$(gh api "repos/$REPO/actions/runners" -q ".runners[] | select(.name == \"$n\") | .id")"
      [ -n "$id" ] && gh api -X DELETE "repos/$REPO/actions/runners/$id"
      docker run --rm -v "$n:$RHOME" --entrypoint rm "$IMAGE" -rf "$RHOME/.registration"
      echo "removed $n"
    done ;;
  clean)
    docker run --rm -v holon-ci-target:/ci-target --entrypoint sh "$IMAGE" -c 'rm -rf /ci-target/* && du -sh /ci-target' ;;
  status)
    gh api "repos/$REPO/actions/runners" -q '.runners[] | "\(.name)\t\(.status)\tbusy=\(.busy)"'
    echo "CI_RUNNER=$(gh variable get CI_RUNNER --repo "$REPO" 2>/dev/null || echo '(unset: GitHub runners)')" ;;
  *) sed -n 2,11p "$0"; exit 1 ;;
esac
