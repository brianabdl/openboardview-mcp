/*
 * C ABI over OpenBoardView's board file parsers.
 *
 * A parse produces an opaque handle. Every pointer returned by the accessor
 * functions (arrays and strings alike) is owned by that handle and stays valid
 * until obv_board_free() is called on it. Callers are expected to copy what
 * they need and free the handle promptly.
 *
 * Coordinates are in mils (thousandths of an inch), as in OpenBoardView.
 *
 * The parsers use process-global state (setlocale), so parsing is not
 * thread-safe: call obv_board_parse() from one thread at a time.
 */
#ifndef OBV_MCP_FFI_H
#define OBV_MCP_FFI_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Mirrors BRDPartMountingSide / BRDPinSide. */
enum {
	OBV_SIDE_BOTH   = 0,
	OBV_SIDE_BOTTOM = 1,
	OBV_SIDE_TOP    = 2,
};

/* Mirrors BRDPartType. */
enum {
	OBV_PART_SMD          = 0,
	OBV_PART_THROUGH_HOLE = 1,
};

typedef struct {
	int32_t x;
	int32_t y;
} obv_point;

typedef struct {
	obv_point a;
	obv_point b;
} obv_segment;

typedef struct {
	const char *name;    /* never NULL */
	const char *mfgcode; /* never NULL, may be empty */
	uint8_t mounting_side;
	uint8_t part_type;
	uint32_t end_of_pins;
	obv_point p1;
	obv_point p2;
} obv_part;

typedef struct {
	obv_point pos;
	int32_t probe;
	uint32_t part; /* 1-based index into the parts array */
	uint8_t side;
	double radius;
	const char *net;  /* never NULL, may be empty */
	const char *snum; /* NULL when the format has no pin numbers */
	const char *name; /* NULL when the format has no pin names */
} obv_pin;

typedef struct {
	uint32_t probe;
	obv_point pos;
	uint8_t side;
	const char *net; /* never NULL */
} obv_nail;

/* Decryption keys for the encrypted formats. NULL key pointers mean "not set". */
typedef struct {
	const uint32_t *fz_key;  /* 44 words, or NULL */
	const uint32_t *cae_key; /* 44 words, or NULL */
	uint64_t xzz_key;
	uint8_t has_xzz_key;
} obv_keys;

typedef struct obv_board obv_board;

/*
 * Parses the board file at `path` (UTF-8). Returns NULL only if memory could
 * not be allocated. Otherwise check obv_board_valid() and obv_board_error().
 */
obv_board *obv_board_parse(const char *path, const obv_keys *keys);
void obv_board_free(obv_board *board);

int obv_board_valid(const obv_board *board);
/* Never NULL; empty when there is no error. */
const char *obv_board_error(const obv_board *board);
/* Short format identifier such as "brd", "fz" or "gencad"; empty if unrecognized. */
const char *obv_board_format(const obv_board *board);

const obv_point *obv_board_outline_points(const obv_board *board, size_t *count);
const obv_segment *obv_board_outline_segments(const obv_board *board, size_t *count);
const obv_part *obv_board_parts(const obv_board *board, size_t *count);
const obv_pin *obv_board_pins(const obv_board *board, size_t *count);
const obv_nail *obv_board_nails(const obv_board *board, size_t *count);

/* Version string of the vendored OpenBoardView sources. */
const char *obv_version(void);

#ifdef __cplusplus
}
#endif

#endif /* OBV_MCP_FFI_H */
