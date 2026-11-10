#!/usr/bin/env bash
# PATH-injected fake `npm` / `pnpm` / `bun` / `git` / `corepack` / `mise` for the
# package-manager integration tests. Every invocation is appended to $KIANA_FAKE_LOG in a
# machine-parseable form:
#
#   CMD <basename>
#   CWD <cwd>
#   ARG <one argv item>
#   ...
#   END
#
# Behavior is driven by KIANA_FAKE_* environment variables; see tests/common/mod.rs.
set -u

program="$(basename "$0")"

if [ -n "${KIANA_FAKE_LOG:-}" ]; then
	block="CMD $program"$'\n'"CWD $PWD"$'\n'
	for argument in "$@"; do
		block+="ARG $argument"$'\n'
	done
	block+="END"$'\n'
	# 并发调用会同时追加同一个日志文件；必须把整块作为一次原子追加，否则不同
	# 进程的 CMD/CWD/ARG/END 行会交错，测试解析器会漏掉调用。优先用 flock；
	# 没有 flock 时退回 mkdir 自旋锁（持有时间只有一次 write）。
	if command -v flock >/dev/null 2>&1; then
		exec 9>>"$KIANA_FAKE_LOG"
		flock 9
		printf '%s' "$block" >&9
		flock -u 9
		exec 9>&-
	else
		lock="$KIANA_FAKE_LOG.lock"
		tries=0
		while ! mkdir "$lock" 2>/dev/null; do
			tries=$((tries + 1))
			if [ "$tries" -ge 400 ]; then
				# 上一个夹具进程被信号杀掉时可能留下陈旧锁；清掉后重试。
				rmdir "$lock" 2>/dev/null || true
				tries=0
			fi
			sleep 0.005
		done
		trap 'rmdir "$lock" 2>/dev/null || true' EXIT INT TERM
		printf '%s' "$block" >>"$KIANA_FAKE_LOG"
		rmdir "$lock" 2>/dev/null || true
		trap - EXIT INT TERM
	fi
fi

if [ -n "${KIANA_FAKE_SLEEP_SECONDS:-}" ]; then
	sleep "$KIANA_FAKE_SLEEP_SECONDS"
fi

matches_fail() {
	if [ -z "${KIANA_FAKE_FAIL_MATCH:-}" ]; then
		return 1
	fi
	for argument in "$@"; do
		case "$argument" in
		*"$KIANA_FAKE_FAIL_MATCH"*) return 0 ;;
		esac
	done
	return 1
}

fail() {
	printf '%s\n' "${1:-simulated failure}" >&2
	exit "${KIANA_FAKE_EXIT:-1}"
}

case "$program" in
mise)
	# `mise exec node@20 -- npm ...`：允许按 node 版本伪造 `npm root -g` 输出。
	if [ -n "${KIANA_FAKE_MISE_ROOT_MAP:-}" ] && [ -f "$KIANA_FAKE_MISE_ROOT_MAP" ] && [ "${1:-}" = "exec" ]; then
		version="${2:-}"
		rest="$*"
		case "$rest" in
		*"npm root -g"*)
			line="$(awk -F'|' -v wanted="$version" '$1 == wanted { print $2; exit }' "$KIANA_FAKE_MISE_ROOT_MAP")"
			if [ -n "$line" ]; then
				printf '%s\n' "$line"
				exit 0
			fi
			;;
		esac
	fi
	# `mise exec node@20 -- npm ...` → exec the command after `--`.
	found=0
	while [ $# -gt 0 ]; do
		if [ "$1" = "--" ]; then
			found=1
			shift
			break
		fi
		shift
	done
	if [ "$found" -eq 1 ] && [ $# -gt 0 ]; then
		exec "$@"
	fi
	exit 0
	;;
corepack)
	# `corepack pnpm ...` → exec the manager with the remaining args.
	if [ $# -gt 0 ]; then
		exec "$@"
	fi
	exit 0
	;;
git)
	if matches_fail "$@"; then
		fail "simulated git failure"
	fi
	command="${1:-}"
	shift || true
	case "$command" in
	clone)
		target="${2:-}"
		if [ -n "${KIANA_FAKE_GIT_CLONE_FAIL:-}" ]; then
			fail "simulated git clone failure"
		fi
		if [ -n "$target" ]; then
			mkdir -p "$target"
			if [ -n "${KIANA_FAKE_GIT_CLONE_PACKAGE_JSON:-}" ]; then
				printf '%s' "$KIANA_FAKE_GIT_CLONE_PACKAGE_JSON" >"$target/package.json"
			fi
			if [ -n "${KIANA_FAKE_GIT_CLONE_FILE:-}" ]; then
				mkdir -p "$(dirname "$target/$KIANA_FAKE_GIT_CLONE_FILE")"
				printf '%s' "${KIANA_FAKE_GIT_CLONE_FILE_CONTENT:-}" >"$target/$KIANA_FAKE_GIT_CLONE_FILE"
			fi
		fi
		;;
	rev-parse)
		if [ -n "${KIANA_FAKE_GIT_REV_PARSE_FAIL:-}" ]; then
			for argument in "$@"; do
				case "$argument" in
				*"$KIANA_FAKE_GIT_REV_PARSE_FAIL"*) fail "fatal: ambiguous argument" ;;
				esac
			done
		fi
		first="${1:-}"
		case "$first" in
		HEAD) printf '%s\n' "${KIANA_FAKE_GIT_LOCAL_HEAD:-}" ;;
		--abbrev-ref) printf '%s\n' "${KIANA_FAKE_GIT_UPSTREAM:-origin/main}" ;;
		*'^{commit}') printf '%s\n' "${KIANA_FAKE_GIT_TARGET_HEAD:-}" ;;
		'@{upstream}' | 'origin/HEAD') printf '%s\n' "${KIANA_FAKE_GIT_UPSTREAM_HEAD:-}" ;;
		*) printf '%s\n' "" ;;
		esac
		;;
	symbolic-ref)
		printf '%s\n' "${KIANA_FAKE_GIT_SYMBOLIC_REF:-}"
		;;
	ls-remote)
		printf '%s\n' "${KIANA_FAKE_GIT_LS_REMOTE:-}"
		;;
	clean)
		if [ -n "${KIANA_FAKE_GIT_CLEAN_FAIL:-}" ]; then
			fail "simulated clean failure"
		fi
		if [ -n "${KIANA_FAKE_GIT_CLEAN_WRITE_PATH:-}" ]; then
			printf '%s' "${KIANA_FAKE_GIT_CLEAN_WRITE_CONTENT:-}" >"$KIANA_FAKE_GIT_CLEAN_WRITE_PATH"
		fi
		;;
	reset)
		if [ -n "${KIANA_FAKE_GIT_RESET_WRITE_PATH:-}" ]; then
			printf '%s' "${KIANA_FAKE_GIT_RESET_WRITE_CONTENT:-}" >"$KIANA_FAKE_GIT_RESET_WRITE_PATH"
		fi
		;;
	*) ;;
	esac
	exit 0
	;;
npm | pnpm | bun)
	if matches_fail "$@"; then
		fail "${KIANA_FAKE_FAIL_STDERR:-simulated npm failure}"
	fi
	action=""
	for argument in "$@"; do
		case "$argument" in
		-*) ;;
		*)
			action="$argument"
			break
			;;
		esac
	done
	# `pm bin -g` for bun.
	if [ "${1:-}" = "pm" ] && [ "${2:-}" = "bin" ]; then
		printf '%s\n' "${KIANA_FAKE_PM_BIN:-/nonexistent/bin}"
		exit 0
	fi
	case "$action" in
	view)
		if [ -n "${KIANA_FAKE_VIEW_FAIL:-}" ]; then
			fail "simulated registry failure"
		fi
		if [ -n "${KIANA_FAKE_VIEW_MAP:-}" ] && [ -f "$KIANA_FAKE_VIEW_MAP" ]; then
			spec="${2:-}"
			line="$(awk -F'|' -v wanted="$spec" '$1 == wanted { print $2; exit }' "$KIANA_FAKE_VIEW_MAP")"
			if [ "$line" = "FAIL" ]; then
				fail "registry unavailable"
			fi
			if [ -n "$line" ]; then
				printf '%s\n' "$line"
				exit 0
			fi
		fi
		printf '%s\n' "${KIANA_FAKE_VIEW_OUTPUT:-1.2.3}"
		;;
	root)
		printf '%s\n' "${KIANA_FAKE_NPM_ROOT:-/nonexistent/node_modules}"
		;;
	list)
		printf '%s\n' "${KIANA_FAKE_PNPM_LIST:-[]}"
		;;
	install)
		spec="${2:-}"
		root=""
		previous=""
		for argument in "$@"; do
			if [ "$previous" = "--prefix" ] || [ "$previous" = "--cwd" ]; then
				root="$argument"
			fi
			previous="$argument"
		done
		if [ -n "${KIANA_FAKE_INSTALL_PACKAGE_JSON:-}" ] && [ -n "$spec" ] && [ -n "$root" ]; then
			name="$spec"
			case "$name" in
			@*)
				scope="${name%%/*}"
				rest="${name#*/}"
				name="$scope/${rest%%@*}"
				;;
			*)
				name="${name%%@*}"
				;;
			esac
			package_dir="$root/node_modules/$name"
			mkdir -p "$package_dir"
			printf '%s' "$KIANA_FAKE_INSTALL_PACKAGE_JSON" >"$package_dir/package.json"
			if [ -n "${KIANA_FAKE_INSTALL_FILE:-}" ]; then
				mkdir -p "$(dirname "$package_dir/$KIANA_FAKE_INSTALL_FILE")"
				printf 'export default function() {}' >"$package_dir/$KIANA_FAKE_INSTALL_FILE"
			fi
		fi
		;;
	*) ;;
	esac
	exit 0
	;;
*)
	exit 0
	;;
esac
