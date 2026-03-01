#!/usr/bin/env bash
set -euo pipefail

OTTER_URL="${OTTER_URL:-http://localhost:8080}"

pretty_print() {
  if command -v jq >/dev/null 2>&1; then
    jq .
  else
    cat
  fi
}

request() {
  local method="$1"
  local path="$2"
  local data="${3:-}"

  if [[ -n "${data}" ]]; then
    curl -sS -X "${method}" \
      -H "Content-Type: application/json" \
      "${OTTER_URL}${path}" \
      -d "${data}"
  else
    curl -sS -X "${method}" "${OTTER_URL}${path}"
  fi
}

usage() {
  cat <<'EOF'
otter-cli.sh - Simple CLI to test Otter API endpoints

Environment:
  OTTER_URL (default: http://localhost:8080)

Usage:
  ./scripts/otter-cli.sh health
  ./scripts/otter-cli.sh projects list
  ./scripts/otter-cli.sh projects create <name> [description]
  ./scripts/otter-cli.sh workspaces list
  ./scripts/otter-cli.sh workspaces create <project_id> <name> <root_path>
  ./scripts/otter-cli.sh prompts enqueue <workspace_id> <prompt> [priority]
  ./scripts/otter-cli.sh jobs get <job_id>
  ./scripts/otter-cli.sh jobs events <job_id>
  ./scripts/otter-cli.sh jobs cancel <job_id>
  ./scripts/otter-cli.sh queue list [limit] [offset]
  ./scripts/otter-cli.sh queue reprioritize <job_id> <priority>
  ./scripts/otter-cli.sh history [limit]
  ./scripts/otter-cli.sh smoke
EOF
}

cmd_health() {
  request "GET" "/healthz"
  echo
}

cmd_projects() {
  local action="${1:-}"
  case "${action}" in
    list)
      request "GET" "/v1/projects" | pretty_print
      ;;
    create)
      local name="${2:-}"
      local description="${3:-}"
      if [[ -z "${name}" ]]; then
        echo "projects create requires: <name> [description]" >&2
        exit 1
      fi
      local payload
      if [[ -n "${description}" ]]; then
        payload="$(printf '{"name":"%s","description":"%s"}' "${name}" "${description}")"
      else
        payload="$(printf '{"name":"%s"}' "${name}")"
      fi
      request "POST" "/v1/projects" "${payload}" | pretty_print
      ;;
    *)
      echo "Unknown projects action: ${action}" >&2
      exit 1
      ;;
  esac
}

cmd_workspaces() {
  local action="${1:-}"
  case "${action}" in
    list)
      request "GET" "/v1/workspaces" | pretty_print
      ;;
    create)
      local project_id="${2:-}"
      local name="${3:-}"
      local root_path="${4:-}"
      if [[ -z "${project_id}" || -z "${name}" || -z "${root_path}" ]]; then
        echo "workspaces create requires: <project_id> <name> <root_path>" >&2
        exit 1
      fi
      local payload
      payload="$(printf '{"project_id":"%s","name":"%s","root_path":"%s"}' "${project_id}" "${name}" "${root_path}")"
      request "POST" "/v1/workspaces" "${payload}" | pretty_print
      ;;
    *)
      echo "Unknown workspaces action: ${action}" >&2
      exit 1
      ;;
  esac
}

cmd_prompts() {
  local action="${1:-}"
  case "${action}" in
    enqueue)
      local workspace_id="${2:-}"
      local prompt="${3:-}"
      local priority="${4:-100}"
      if [[ -z "${workspace_id}" || -z "${prompt}" ]]; then
        echo "prompts enqueue requires: <workspace_id> <prompt> [priority]" >&2
        exit 1
      fi
      local payload
      payload="$(printf '{"workspace_id":"%s","prompt":"%s","priority":%s}' "${workspace_id}" "${prompt}" "${priority}")"
      request "POST" "/v1/prompts" "${payload}" | pretty_print
      ;;
    *)
      echo "Unknown prompts action: ${action}" >&2
      exit 1
      ;;
  esac
}

cmd_jobs() {
  local action="${1:-}"
  local job_id="${2:-}"
  if [[ -z "${job_id}" ]]; then
    echo "jobs requires: <get|events|cancel> <job_id>" >&2
    exit 1
  fi
  case "${action}" in
    get)
      request "GET" "/v1/jobs/${job_id}" | pretty_print
      ;;
    events)
      request "GET" "/v1/jobs/${job_id}/events" | pretty_print
      ;;
    cancel)
      request "POST" "/v1/jobs/${job_id}/cancel"
      echo
      ;;
    *)
      echo "Unknown jobs action: ${action}" >&2
      exit 1
      ;;
  esac
}

cmd_queue() {
  local action="${1:-}"
  case "${action}" in
    list)
      local limit="${2:-100}"
      local offset="${3:-0}"
      request "GET" "/v1/queue?limit=${limit}&offset=${offset}" | pretty_print
      ;;
    reprioritize)
      local job_id="${2:-}"
      local priority="${3:-}"
      if [[ -z "${job_id}" || -z "${priority}" ]]; then
        echo "queue reprioritize requires: <job_id> <priority>" >&2
        exit 1
      fi
      local payload
      payload="$(printf '{"priority":%s}' "${priority}")"
      request "PATCH" "/v1/queue/${job_id}" "${payload}"
      echo
      ;;
    *)
      echo "Unknown queue action: ${action}" >&2
      exit 1
      ;;
  esac
}

cmd_history() {
  local limit="${1:-100}"
  request "GET" "/v1/history?limit=${limit}" | pretty_print
}

cmd_smoke() {
  echo "== health =="
  cmd_health
  echo "== queue (top 10) =="
  cmd_queue list 10 0
  echo "== history (top 10) =="
  cmd_history 10
}

main() {
  local cmd="${1:-}"
  case "${cmd}" in
    health)
      cmd_health
      ;;
    projects)
      shift || true
      cmd_projects "${@:-}"
      ;;
    workspaces)
      shift || true
      cmd_workspaces "${@:-}"
      ;;
    prompts)
      shift || true
      cmd_prompts "${@:-}"
      ;;
    jobs)
      shift || true
      cmd_jobs "${@:-}"
      ;;
    queue)
      shift || true
      cmd_queue "${@:-}"
      ;;
    history)
      shift || true
      cmd_history "${@:-}"
      ;;
    smoke)
      cmd_smoke
      ;;
    -h|--help|help|"")
      usage
      ;;
    *)
      echo "Unknown command: ${cmd}" >&2
      usage
      exit 1
      ;;
  esac
}

main "$@"
