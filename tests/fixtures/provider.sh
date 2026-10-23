#!/bin/sh
# Test double for both provider CLIs. Never contacts a model.
out=""
prior=""
for arg in "$@"; do
    if [ "$prior" = "--output-last-message" ]; then out="$arg"; fi
    prior="$arg"
done
prompt=$(cat)
case "$MOCK_MODE" in
    timeout) printf '%s\n' "$$" > .ai-lead/mock-provider.pid.tmp; mv .ai-lead/mock-provider.pid.tmp .ai-lead/mock-provider.pid; sleep 10; exit 0 ;;
    auth-error) printf '%s\n' '{"type":"result","subtype":"error_during_execution","is_error":true,"result":"Authentication required"}'; exit 0 ;;
esac
case "$prompt" in
    *"You are backend. Lead:"*)
        printf '%s\n' backend >> .ai-lead/mock-calls.log
        case "$MOCK_MODE" in
            worker-error) printf '%s\n' 'backend failed' >&2; exit 1 ;;
        esac
        payload='{"summary":"Backend implemented contract","messages":[{"to":"frontend","body":"CONTRACT_V1"},{"to":"lead","body":"Backend contract ready"}],"memory":{"facts":["Backend remembers CONTRACT_V1"],"next_steps":["Verify frontend integration"],"blockers":[],"shared":["Backend published CONTRACT_V1"]}}'
        ;;
    *"You are frontend. Lead:"*)
        printf '%s\n' frontend >> .ai-lead/mock-calls.log
        if [ "$MOCK_MODE" = "pause-frontend" ]; then
            printf '%s\n' "$$" > .ai-lead/mock-frontend.pid.tmp
            mv .ai-lead/mock-frontend.pid.tmp .ai-lead/mock-frontend.pid
            sleep 10
        fi
        case "$prompt" in
            *CONTRACT_V1*) payload='{"summary":"Frontend received CONTRACT_V1 and verified integration","messages":[],"memory":{"facts":["Frontend consumed CONTRACT_V1"],"next_steps":["Review accessibility"],"blockers":[],"shared":["Frontend integration verified with fixture"]}}' ;;
            *) printf '%s\n' 'peer message missing' >&2; exit 1 ;;
        esac
        ;;
    *)
        printf '%s\n' lead >> .ai-lead/mock-calls.log
        case "$prompt" in
            *'"error":'*) payload='{"action":"blocked","assignments":[],"messages":[],"summary":"Leader acknowledged specialist failure; incomplete implementation","memory":{"facts":["Lead preserves the engineering plan"],"next_steps":["Integrate specialist reports"],"blockers":["Specialist failure"],"shared":["Lead decision: blocked"]}}' ;;
            *"Frontend received CONTRACT_V1"*) payload='{"action":"finish","assignments":[],"messages":[],"summary":"Leader verified all reports and approved result","memory":{"facts":["Lead preserves the engineering plan"],"next_steps":[],"blockers":[],"shared":["Lead decision: finish"]}}' ;;
            *) payload='{"action":"delegate","assignments":[{"agent":"backend","task":"Implement API contract"},{"agent":"frontend","task":"Consume backend contract"}],"messages":[{"to":"backend","body":"Define contract before frontend"}],"summary":"Plan backend then frontend","memory":{"facts":["Lead preserves the engineering plan"],"next_steps":["Integrate specialist reports"],"blockers":[],"shared":["Lead decision: delegate"]}}' ;;
        esac
        ;;
esac
if [ -n "$out" ]; then
    printf '%s\n' "$payload" > "$out"
    printf '%s\n' '{"type":"item.completed","item":{"type":"agent_message","text":"mock progress"}}'
else
    printf '{"type":"result","subtype":"success","is_error":false,"structured_output":%s}\n' "$payload"
fi
