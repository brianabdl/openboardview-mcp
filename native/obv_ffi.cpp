// C ABI over OpenBoardView's board file parsers. See obv_ffi.h.

#include "obv_ffi.h"

#include <array>
#include <exception>
#include <memory>
#include <new>
#include <string>
#include <vector>

#include "FileFormats/ADFile.h"
#include "FileFormats/ASCFile.h"
#include "FileFormats/BDVFile.h"
#include "FileFormats/BRD2File.h"
#include "FileFormats/BRDAllegroFile.h"
#include "FileFormats/BRDFile.h"
#include "FileFormats/BVR3File.h"
#include "FileFormats/BVRFile.h"
#include "FileFormats/CADFile.h"
#include "FileFormats/CAEFile.h"
#include "FileFormats/CSTFile.h"
#include "FileFormats/FZFile.h"
#include "FileFormats/GenCADFile.h"
#include "FileFormats/XZZPCBFile.h"
#include "utils.h"

#ifndef OBV_VERSION
#define OBV_VERSION "unknown"
#endif

struct obv_board {
	std::unique_ptr<BRDFileBase> file;
	std::string format;
	std::string error;

	std::vector<obv_point> outline_points;
	std::vector<obv_segment> outline_segments;
	std::vector<obv_part> parts;
	std::vector<obv_pin> pins;
	std::vector<obv_nail> nails;
};

namespace {

obv_point to_point(const BRDPoint &p) {
	return obv_point{p.x, p.y};
}

const char *non_null(const char *s) {
	return s ? s : "";
}

std::array<uint32_t, 44> to_key(const uint32_t *words) {
	std::array<uint32_t, 44> key{};
	if (words) {
		for (size_t i = 0; i < key.size(); ++i) key[i] = words[i];
	}
	return key;
}

// Same detection order as BoardView::LoadFile in OpenBoardView, so a file is
// interpreted exactly as the GUI would interpret it.
std::unique_ptr<BRDFileBase> detect_and_parse(const filesystem::path &path, std::vector<char> &buffer, const obv_keys *keys, std::string &format) {
	if (check_fileext(path, ".fz")) { // Encrypted, so trust the extension.
		auto file = std::make_unique<FZFile>();
		file->parse(buffer, to_key(keys ? keys->fz_key : nullptr));
		format = "fz";
		return file;
	}
	if (check_fileext(path, ".cae")) { // Encrypted, so trust the extension.
		auto file = std::make_unique<CAEFile>();
		file->parse(buffer, to_key(keys ? keys->cae_key : nullptr));
		format = "cae";
		return file;
	}
	if (check_fileext(path, ".bom") || check_fileext(path, ".asc")) {
		format = "asc";
		return std::make_unique<ASCFile>(buffer, path);
	}
	if (GenCADFile::verifyFormat(buffer)) {
		format = "gencad";
		return std::make_unique<GenCADFile>(buffer);
	}
	if (ADFile::verifyFormat(buffer)) {
		format = "altium";
		return std::make_unique<ADFile>(buffer);
	}
	if (CADFile::verifyFormat(buffer)) {
		format = "cad";
		return std::make_unique<CADFile>(buffer);
	}
	if (check_fileext(path, ".cst")) {
		format = "cst";
		return std::make_unique<CSTFile>(buffer);
	}
	if (BRDFile::verifyFormat(buffer)) {
		format = "brd";
		return std::make_unique<BRDFile>(buffer);
	}
	if (BRD2File::verifyFormat(buffer)) {
		format = "brd2";
		return std::make_unique<BRD2File>(buffer);
	}
	if (BDVFile::verifyFormat(buffer)) {
		format = "bdv";
		return std::make_unique<BDVFile>(buffer);
	}
	if (BVRFile::verifyFormat(buffer)) {
		format = "bvr";
		return std::make_unique<BVRFile>(buffer);
	}
	if (BVR3File::verifyFormat(buffer)) {
		format = "bvr3";
		return std::make_unique<BVR3File>(buffer);
	}
	if (BRDAllegroFile::verifyFormat(buffer)) {
		format = "allegro";
		return std::make_unique<BRDAllegroFile>(buffer);
	}
	if (XZZPCBFile::verifyFormat(buffer)) {
		format = "xzzpcb";
		const bool has_key = keys && keys->has_xzz_key;
		return std::make_unique<XZZPCBFile>(buffer, has_key ? keys->xzz_key : 0);
	}
	return nullptr;
}

void flatten(obv_board &board) {
	const BRDFileBase &file = *board.file;

	board.outline_points.reserve(file.format.size());
	for (const auto &p : file.format) board.outline_points.push_back(to_point(p));

	board.outline_segments.reserve(file.outline_segments.size());
	for (const auto &s : file.outline_segments) board.outline_segments.push_back(obv_segment{to_point(s.first), to_point(s.second)});

	board.parts.reserve(file.parts.size());
	for (const auto &part : file.parts) {
		obv_part out{};
		out.name          = non_null(part.name);
		out.mfgcode       = part.mfgcode.c_str();
		out.mounting_side = static_cast<uint8_t>(part.mounting_side);
		out.part_type     = static_cast<uint8_t>(part.part_type);
		out.end_of_pins   = part.end_of_pins;
		out.p1            = to_point(part.p1);
		out.p2            = to_point(part.p2);
		board.parts.push_back(out);
	}

	board.pins.reserve(file.pins.size());
	for (const auto &pin : file.pins) {
		obv_pin out{};
		out.pos    = to_point(pin.pos);
		out.probe  = pin.probe;
		out.part   = pin.part;
		out.side   = static_cast<uint8_t>(pin.side);
		out.radius = pin.radius;
		out.net    = non_null(pin.net);
		out.snum   = pin.snum;
		out.name   = pin.name;
		board.pins.push_back(out);
	}

	board.nails.reserve(file.nails.size());
	for (const auto &nail : file.nails) {
		obv_nail out{};
		out.probe = nail.probe;
		out.pos   = to_point(nail.pos);
		out.side  = static_cast<uint8_t>(nail.side);
		out.net   = non_null(nail.net);
		board.nails.push_back(out);
	}
}

template <typename T>
const T *expose(const std::vector<T> &items, size_t *count) {
	if (count) *count = items.size();
	return items.empty() ? nullptr : items.data();
}

} // namespace

static_assert(static_cast<int>(BRDPartMountingSide::Both) == OBV_SIDE_BOTH, "side enum mismatch");
static_assert(static_cast<int>(BRDPartMountingSide::Bottom) == OBV_SIDE_BOTTOM, "side enum mismatch");
static_assert(static_cast<int>(BRDPartMountingSide::Top) == OBV_SIDE_TOP, "side enum mismatch");
static_assert(static_cast<int>(BRDPinSide::Both) == OBV_SIDE_BOTH, "side enum mismatch");
static_assert(static_cast<int>(BRDPinSide::Bottom) == OBV_SIDE_BOTTOM, "side enum mismatch");
static_assert(static_cast<int>(BRDPinSide::Top) == OBV_SIDE_TOP, "side enum mismatch");
static_assert(static_cast<int>(BRDPartType::SMD) == OBV_PART_SMD, "part type enum mismatch");
static_assert(static_cast<int>(BRDPartType::ThroughHole) == OBV_PART_THROUGH_HOLE, "part type enum mismatch");

extern "C" obv_board *obv_board_parse(const char *path, const obv_keys *keys) {
	obv_board *board = new (std::nothrow) obv_board();
	if (!board) return nullptr;

	// No C++ exception may cross the C boundary.
	try {
		if (!path) {
			board->error = "No file path given.";
			return board;
		}
		const filesystem::path filepath = filesystem::u8path(path);

		std::vector<char> buffer = file_as_buffer(filepath, board->error);
		if (buffer.empty()) {
			if (board->error.empty()) board->error = "File is empty.";
			return board;
		}

		board->file = detect_and_parse(filepath, buffer, keys, board->format);
		if (!board->file) {
			board->error = "Unrecognized file format.";
			return board;
		}
		if (!board->file->valid) {
			board->error = board->file->error_msg.empty() ? "The file could not be parsed." : board->file->error_msg;
			return board;
		}
		flatten(*board);
	} catch (const std::exception &e) {
		board->file.reset();
		board->error = std::string("Parser raised an exception: ") + e.what();
	} catch (...) {
		board->file.reset();
		board->error = "Parser raised an unknown exception.";
	}
	return board;
}

extern "C" void obv_board_free(obv_board *board) {
	delete board;
}

extern "C" int obv_board_valid(const obv_board *board) {
	return board && board->file && board->file->valid ? 1 : 0;
}

extern "C" const char *obv_board_error(const obv_board *board) {
	return board ? board->error.c_str() : "No board.";
}

extern "C" const char *obv_board_format(const obv_board *board) {
	return board ? board->format.c_str() : "";
}

extern "C" const obv_point *obv_board_outline_points(const obv_board *board, size_t *count) {
	return expose(board->outline_points, count);
}

extern "C" const obv_segment *obv_board_outline_segments(const obv_board *board, size_t *count) {
	return expose(board->outline_segments, count);
}

extern "C" const obv_part *obv_board_parts(const obv_board *board, size_t *count) {
	return expose(board->parts, count);
}

extern "C" const obv_pin *obv_board_pins(const obv_board *board, size_t *count) {
	return expose(board->pins, count);
}

extern "C" const obv_nail *obv_board_nails(const obv_board *board, size_t *count) {
	return expose(board->nails, count);
}

extern "C" const char *obv_version(void) {
	return OBV_VERSION;
}
