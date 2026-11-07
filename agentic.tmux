#!/bin/sh
# tmux-agentic: an overview of the coding agents running in your tmux panes.
# Run by TPM (set -g @plugin ...), or without TPM from tmux.conf, after your own status-right line:
#   run-shell ~/.tmux/plugins/tmux-agentic/agentic.tmux
# Safe to run again (config reloads, a moved checkout): it replaces what it set before.

dir=$(cd "$(dirname "$0")" && pwd)
repo=oluevaera/tmux-agentic # where the releases are
binary=$dir/bin/agentic
[ ! -x "$dir/target/release/agentic" ] || binary=$dir/target/release/agentic # your own cargo build wins, for development
agents="'$(printf '%s' "$binary" | sed "s/'/'\\\\''/g")'" # quoted for the shell tmux runs commands in

# The release this plugin installs, fetched in the background so tmux stays usable; the keys below work once it is here.
# The release branch TPM installs has it written on the next line by the release job; a source checkout has Cargo.toml's.
# A plugin update changes the version, which fetches the new one.
version=0.1.0
[ -n "$version" ] || version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$dir/Cargo.toml" 2>/dev/null | head -1)
case $binary in
*/bin/agentic)
	if [ "$("$binary" --version 2>/dev/null)" != "$version" ]; then
		asset=agentic-$(uname -s)-$(uname -m)
		get() { curl -fsL -o "$2" "https://github.com/$repo/releases/download/v$version/$1"; } # one file of the release
		sha256() { if command -v sha256sum >/dev/null; then sha256sum "$1"; else shasum -a 256 "$1"; fi | cut -d' ' -f1; }
		# The build, only if it's the one the release's checksum names: a cut-off or altered download never runs.
		fetch() {
			get "$asset" "$new" && get "$asset.sha256" "$sums" &&
				[ "$(sha256 "$new")" = "$(cut -d' ' -f1 "$sums")" ]
		}
		new=$binary.new.$$ sums=$binary.sha256.$$ # this run's own: another run at once has its own
		(
			if mkdir -p "$dir/bin" && fetch && chmod +x "$new" && mv "$new" "$binary"; then
				rm -f "$sums"
				"$binary" bind
				# The watch tmux restarted while this downloaded is the old build: tmux starts the new one in its place.
				pkill -f "$binary status --watch"
				tmux display-message 'tmux-agentic: ready'
			else
				rm -f "$new" "$sums"
				tmux display-message "tmux-agentic: could not download $asset v$version (see the README to build it yourself)"
			fi
		) >/dev/null 2>&1 & # its own output: run-shell waits for every holder of its own to finish
	fi
	;;
esac

# Keys: prefix + a opens the board, prefix + b goes back; a key without the prefix only if you set one. The binary binds them as
# ~/.config/tmux-agentic/config.toml says, and again whenever that changes; a first install, once it has downloaded.
"$binary" bind 2>/dev/null

# Status bar: "● 1 needs you ● 2 working" where status-right has #{agentic_status}, else in front of it.
# A segment from an earlier run (or an older checkout path) turns back into the placeholder first, so reruns keep its
# place and never add a second one.
segment="#($agents status --watch)#[default]"
right=$(tmux show -gv status-right | sed -E "s/#\([^)]*agentic'* status --watch\)#\[default\]/#{agentic_status}/g")
case $right in
*'#{agentic_status}'*)
	# Word for word, by the shell's own cutting: awk would read & in the path as the matched text, and \ as escapes.
	while case $right in *'#{agentic_status}'*) true ;; *) false ;; esac; do
		right=${right%%'#{agentic_status}'*}$segment${right#*'#{agentic_status}'}
	done
	;;
*) right=$segment$right ;;
esac
tmux set -g status-right "$right"
pkill -f "$binary status --watch" 2>/dev/null # a new build takes over: tmux starts the command again
[ "$(tmux show -gv status-right-length)" -ge 100 ] || tmux set -g status-right-length 100 # room for the counts
