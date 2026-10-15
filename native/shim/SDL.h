/*
 * Minimal stand-in for <SDL.h>.
 *
 * OpenBoardView's file-format parsers only use SDL for logging. This header
 * provides just enough of that API so the parsers can be compiled without
 * SDL. Messages go to stderr, which the MCP server captures from its parse
 * worker and forwards to its own log.
 */
#ifndef OBV_MCP_SDL_SHIM_H
#define OBV_MCP_SDL_SHIM_H

#include <stdarg.h>
#include <stdio.h>

#ifdef __cplusplus
extern "C" {
#endif

enum {
	SDL_LOG_CATEGORY_APPLICATION = 0,
	SDL_LOG_CATEGORY_ERROR       = 1,
};

static inline void obv_mcp_sdl_log(const char *level, const char *fmt, va_list args) {
	fprintf(stderr, "[obv %s] ", level);
	vfprintf(stderr, fmt, args);
	fputc('\n', stderr);
}

#define OBV_MCP_DEFINE_SDL_LOG(name, level)                       \
	static inline void name(int category, const char *fmt, ...) { \
		(void)category;                                           \
		va_list args;                                             \
		va_start(args, fmt);                                      \
		obv_mcp_sdl_log(level, fmt, args);                        \
		va_end(args);                                             \
	}

OBV_MCP_DEFINE_SDL_LOG(SDL_LogError, "error")
OBV_MCP_DEFINE_SDL_LOG(SDL_LogWarn, "warn")
OBV_MCP_DEFINE_SDL_LOG(SDL_LogInfo, "info")
OBV_MCP_DEFINE_SDL_LOG(SDL_LogDebug, "debug")

#undef OBV_MCP_DEFINE_SDL_LOG

#ifdef __cplusplus
}
#endif

#endif /* OBV_MCP_SDL_SHIM_H */
