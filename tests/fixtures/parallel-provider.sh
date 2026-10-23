#!/bin/sh
# Local deterministic provider double. The test, rather than elapsed time,
# releases each running specialist through filesystem barriers.
set -eu
out=""
prior=""
for arg in "$@"; do
    if [ "$prior" = "--output-last-message" ]; then out="$arg"; fi
    prior="$arg"
done
prompt=$(cat)
root=.ai-lead/parallel
mkdir -p "$root"
role=lead
for candidate in backend designer frontend tester reviewer; do
    case "$prompt" in *"You are $candidate. Lead:"*) role=$candidate ;; esac
done
printf '%s\n' "$$" > "$root/$role.pid"
printf '%s\n' "$role" >> "$root/calls"
memory='"memory":{"facts":[],"next_steps":[],"blockers":[],"shared":[]}'
if [ "$role" = lead ]; then
    count=0
    if [ -f "$root/lead-count" ]; then count=$(cat "$root/lead-count"); fi
    count=$((count + 1))
    printf '%s\n' "$count" > "$root/lead-count"
    assignments='[]'
    action=finish
    summary='All fixture tasks completed'
    limits='[]'
    if [ "$count" -eq 1 ]; then
        action=delegate
        summary='Independent backend and collaborative interface delivery'
        case "$PARALLEL_MODE" in
            steering)
                action=finish
                assignments='[]'
                summary=STALE_COMPLETION
                : > "$root/lead.planning"
                remaining=1000
                while [ ! -f "$root/release-lead" ]; do
                    remaining=$((remaining - 1)); test "$remaining" -gt 0 || exit 1
                    sleep 0.02
                done ;;
            live-review)
                assignments='[{"agent":"backend","task":"Implement server only","depends_on":[],"files":["server/"]},{"agent":"frontend","task":"Implement client only","depends_on":[],"files":["web/components/"]},{"agent":"designer","task":"Implement visual primitives","depends_on":[],"files":["web/styles/"]},{"agent":"tester","task":"Write contract tests during implementation","depends_on":[],"files":["tests/"]},{"agent":"reviewer","task":"Review logic during implementation","depends_on":[],"files":[]}]' ;;
            independent|resume|capacity|live)
                assignments='[{"agent":"backend","task":"Implement server only","depends_on":[],"files":["server/"]},{"agent":"designer","task":"Implement interface styles","depends_on":[],"files":["web/styles/"]},{"agent":"frontend","task":"Implement interface components","depends_on":[],"files":["web/components/"]}]' ;;
            dependency)
                assignments='[{"agent":"backend","task":"Implement server only","depends_on":[],"files":["server/"]},{"agent":"designer","task":"Create visual contract","depends_on":[],"files":["web/styles/"]},{"agent":"frontend","task":"Implement visual contract DESIGN_READY","depends_on":["designer"],"files":["web/components/"]}]' ;;
            conflict)
                assignments='[{"agent":"backend","task":"Implement shared source","depends_on":[],"files":["src/"]},{"agent":"frontend","task":"Update nested shared file","depends_on":[],"files":["src/shared.ts"]}]' ;;
            legacy)
                assignments='[{"agent":"backend","task":"Legacy exclusive task"},{"agent":"frontend","task":"Legacy exclusive followup"}]' ;;
            recovery)
                assignments='[{"agent":"backend","task":"Fail initial backend attempt","depends_on":[],"files":["server/"]},{"agent":"frontend","task":"Must skip failed prerequisite","depends_on":["backend"],"files":["web/"]}]' ;;
            invalid-first-plan)
                assignments='[{"agent":"backend","task":"INVALID_CYCLE_BACKEND","depends_on":["frontend"],"files":["server/"]},{"agent":"frontend","task":"INVALID_CYCLE_FRONTEND","depends_on":["backend"],"files":["web/"]}]' ;;
            limits)
                action=finish_with_limits
                summary='Implementation delivered; browser verification remains unavailable'
                limits='["Browser bridge unavailable; responsive behavior was not verified"]' ;;
        esac
    elif [ "$PARALLEL_MODE" = steering ] && [ "$count" -eq 2 ]; then
        case "$prompt" in *STEER_INPUT*) ;; *) echo 'user input lost during lead inference' >&2; exit 1 ;; esac
        action=delegate
        summary='Fresh plan for new user request'
        assignments='[{"agent":"backend","task":"Implement user follow-up server scope","depends_on":[],"files":["server/"]}]'
    elif [ "$PARALLEL_MODE" = invalid-first-plan ] && [ "$count" -eq 2 ]; then
        case "$prompt" in *'COORDINATOR FEEDBACK:'*'dependency cycle'*) ;; *) echo 'missing actionable plan repair feedback' >&2; exit 1 ;; esac
        test "$(cat "$root/calls")" = "$(printf 'lead\nlead')" || { echo 'invalid plan dispatched workers before repair' >&2; exit 1; }
        : > "$root/repair.approved"
        action=delegate
        summary='Corrected independent file ownership and acyclic plan'
        assignments='[{"agent":"backend","task":"Implement corrected server scope","depends_on":[],"files":["server/"]},{"agent":"designer","task":"Implement corrected design scope","depends_on":[],"files":["web/styles/"]},{"agent":"frontend","task":"Implement corrected interface scope","depends_on":[],"files":["web/components/"]}]'
    elif [ "$PARALLEL_MODE" = recovery ] && [ "$count" -eq 2 ]; then
        case "$prompt" in *backend*failed*) ;; *) echo 'lead did not receive failed prerequisite' >&2; exit 1 ;; esac
        action=delegate
        summary='Recover failed backend and then its consumer'
        assignments='[{"agent":"backend","task":"Recover backend implementation","depends_on":[],"files":["server/"]},{"agent":"frontend","task":"Integrate recovered backend","depends_on":["backend"],"files":["web/"]}]'
    fi
    payload="{\"action\":\"$action\",\"assignments\":$assignments,\"messages\":[],\"summary\":\"$summary\",\"brief\":\"SHARED_ARCH_V2: Architecture: modular monolith; separate server and web. Backend owns server, design owns styles, frontend owns components.\",\"verification_limits\":$limits,$memory}"
else
    if [ "$PARALLEL_MODE" = invalid-first-plan ]; then
        test -f "$root/repair.approved" || { echo 'worker received invalid plan' >&2; exit 1; }
        case "$prompt" in *'Task assigned by lead:'*'Implement corrected'*) ;; *) echo 'worker received incorrect repaired assignment' >&2; exit 1 ;; esac
    fi
    case "$prompt" in *SHARED_ARCH_V2*) ;; *) echo 'shared architecture brief missing' >&2; exit 1 ;; esac
    session=$(dirname "$(dirname "$out")")
    count=0
    if [ -f "$root/$role-count" ]; then count=$(cat "$root/$role-count"); fi
    count=$((count + 1))
    printf '%s\n' "$count" > "$root/$role-count"
    if [ "$PARALLEL_MODE" = recovery ] && [ "$role" = backend ] && [ "$count" -eq 1 ]; then
        : > "$root/backend.failed"
        echo 'backend failed fixture prerequisite' >&2
        exit 1
    fi
    if [ "$PARALLEL_MODE" = dependency ] && [ "$role" = frontend ]; then
        test -f "$root/designer.finished" || { echo 'dependency ran too early' >&2; exit 1; }
        case "$prompt" in *'DESIGN_HANDOFF_READY'*) ;; *) echo 'designer peer handoff missing' >&2; exit 1 ;; esac
    fi
    case "$PARALLEL_MODE:$role" in
        conflict:frontend|legacy:frontend)
            test -f "$root/backend.finished" || { echo 'conflicting write scopes overlapped' >&2; exit 1; } ;;
    esac
    if [ "$PARALLEL_MODE" = capacity ]; then
        mkdir "$root/one-running" || { echo 'configured concurrency exceeded' >&2; exit 1; }
    fi
    if [ "$PARALLEL_MODE:$role" = live:frontend ]; then
        remaining=1000
        while :; do
            if [ -f "$session/team/inbox/frontend.json" ]; then
                case "$(cat "$session/team/inbox/frontend.json")" in *LIVE_DESIGN_BEFORE_REPORT*) break ;; esac
            fi
            remaining=$((remaining - 1))
            if [ "$remaining" -le 0 ]; then echo 'live design handoff did not arrive' >&2; exit 1; fi
            sleep 0.02
        done
    fi
    : > "$root/$role.started"
    if [ "$PARALLEL_MODE" = live-review ]; then
        case "$prompt" in *ROLE_BOUNDARY*'LIVE WORK STATUS:'*) ;; *) echo 'runtime role boundary or progress missing' >&2; exit 1 ;; esac
        test -f "$session/team/progress.json" || { echo 'progress snapshot missing' >&2; exit 1; }
        if [ "$role" = reviewer ] || [ "$role" = tester ]; then
            remaining=500
            while [ ! -f "$root/backend.started" ]; do
                remaining=$((remaining - 1)); test "$remaining" -gt 0 || exit 1
                sleep 0.02
            done
            test ! -f "$root/backend.finished" || { echo 'QA/review began only after backend completed' >&2; exit 1; }
        fi
        if [ "$role" = reviewer ]; then
            printf '%s\n' '{"to":"backend","body":"REVIEW_DURING_WRITING: inspect error propagation in the current server chunk"}' >> "$session/team/outbox/reviewer.jsonl"
            : > "$root/review.sent"
        fi
        if [ "$role" = tester ]; then
            mkdir -p tests
            printf '%s\n' '# Contract scenarios authored while server implementation is active' > tests/contract.md
            : > "$root/tests.authored"
        fi
    fi
    if [ "$PARALLEL_MODE:$role" = live:designer ]; then
        printf '%s\n' '{"to":"frontend","body":"LIVE_DESIGN_BEFORE_REPORT: compose responsive section cards"}' >> "$session/team/outbox/designer.jsonl"
    fi
    case "$PARALLEL_MODE" in
        recovery) ;;
        *)
            remaining=1500
            while [ ! -f "$root/release-$role" ]; do
                if [ "$PARALLEL_MODE:$role" = live-review:backend ] && [ -f "$session/team/inbox/backend.json" ]; then
                    case "$(cat "$session/team/inbox/backend.json")" in *REVIEW_DURING_WRITING*) : > "$root/review.received" ;; esac
                fi
                remaining=$((remaining - 1))
                if [ "$remaining" -le 0 ]; then echo "barrier timeout: $role" >&2; exit 1; fi
                sleep 0.02
            done ;;
    esac
    if [ "$PARALLEL_MODE" = capacity ]; then rmdir "$root/one-running"; fi
    : > "$root/$role.finished"
    messages='[]'
    if [ "$role" = designer ]; then messages='[{"to":"frontend","body":"DESIGN_HANDOFF_READY: use shared visual contract"}]'; fi
    payload="{\"summary\":\"$role fixture completed\",\"messages\":$messages,$memory}"
fi
printf '%s\n' "$payload" > "$out"
printf '%s\n' '{"type":"item.completed","item":{"type":"agent_message","text":"fixture progress"}}'
