#!/usr/bin/env bash

get_tmux_option() {
    local option="$1"
    local default_value="$2"
    local option_value
    option_value=$(tmux show-option -gqv "$option")
    if [ -z "$option_value" ]; then
        echo "$default_value"
    else
        echo "$option_value"
    fi
}

main() {
    local key=$(get_tmux_option "@vex-key" "C-v")
    local popup_width=$(get_tmux_option "@vex-popup-width" "80%")
    local popup_height=$(get_tmux_option "@vex-popup-height" "80%")

    tmux bind-key "$key" display-popup -E -w "$popup_width" -h "$popup_height" "vex open"
}

main
