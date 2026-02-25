/*
 * Coil portable bootloader.
 *
 * This is a minimal Windows executable that:
 * 1. Reads a zip archive appended after its own PE data
 * 2. Extracts it to %LOCALAPPDATA%\coil\<AppName>\
 * 3. Launches the real application exe from the extracted dir
 * 4. Forwards the exit code
 *
 * The zip offset is stored in the last 8 bytes of the file:
 *   [4 bytes: zip offset as uint32] [4 bytes: magic 0x434F494C "COIL"]
 *
 * Build: zig cc -O2 -mwindows -o bootloader.exe bootloader.c
 */

#define WIN32_LEAN_AND_MEAN
#ifndef UNICODE
#define UNICODE
#endif
#ifndef _UNICODE
#define _UNICODE
#endif
#include <windows.h>

/* --- Minimal zip reading (local file headers only) --- */

#pragma pack(push, 1)
typedef struct {
    DWORD signature;       /* 0x04034b50 */
    WORD  version;
    WORD  flags;
    WORD  compression;
    WORD  mod_time;
    WORD  mod_date;
    DWORD crc32;
    DWORD compressed_size;
    DWORD uncompressed_size;
    WORD  name_len;
    WORD  extra_len;
} ZipLocalHeader;
#pragma pack(pop)

#define ZIP_LOCAL_SIG 0x04034b50
#define COIL_MAGIC    0x434F494C  /* "COIL" */

/* Forward declarations */
static BOOL ensure_directory(const WCHAR *path);
static BOOL extract_zip(const BYTE *data, DWORD size, const WCHAR *dest);
static void path_combine(WCHAR *out, const WCHAR *dir, const WCHAR *file, int max);
static void ansi_to_wide(const char *src, WCHAR *dst, int max);

int WINAPI wWinMain(HINSTANCE hInst, HINSTANCE hPrev, LPWSTR cmdLine, int nShow) {
    WCHAR exe_path[MAX_PATH];
    WCHAR app_name[MAX_PATH];
    WCHAR extract_dir[MAX_PATH];
    WCHAR target_exe[MAX_PATH];
    WCHAR local_app[MAX_PATH];
    WCHAR *slash, *dot;
    HANDLE hFile, hMap;
    BYTE *file_data;
    DWORD file_size, zip_offset, magic;
    DWORD exit_code = 1;
    STARTUPINFOW si;
    PROCESS_INFORMATION pi;

    /* Get our own path */
    GetModuleFileNameW(NULL, exe_path, MAX_PATH);

    /* Extract app name from exe filename */
    slash = wcsrchr(exe_path, L'\\');
    if (slash) slash++;
    else slash = exe_path;
    wcscpy_s(app_name, MAX_PATH, slash);
    dot = wcsrchr(app_name, L'.');
    if (dot) *dot = 0;

    /* Build extraction path: %LOCALAPPDATA%\coil\AppName\ */
    if (!GetEnvironmentVariableW(L"LOCALAPPDATA", local_app, MAX_PATH))
        GetTempPathW(MAX_PATH, local_app);

    wsprintfW(extract_dir, L"%s\\coil\\%s", local_app, app_name);

    /* Build target exe path */
    wsprintfW(target_exe, L"%s\\%s.exe", extract_dir, app_name);

    /* Memory-map our own exe */
    hFile = CreateFileW(exe_path, GENERIC_READ, FILE_SHARE_READ, NULL,
                        OPEN_EXISTING, 0, NULL);
    if (hFile == INVALID_HANDLE_VALUE) return 1;

    file_size = GetFileSize(hFile, NULL);
    if (file_size < 8) { CloseHandle(hFile); return 1; }

    hMap = CreateFileMappingW(hFile, NULL, PAGE_READONLY, 0, 0, NULL);
    if (!hMap) { CloseHandle(hFile); return 1; }

    file_data = (BYTE *)MapViewOfFile(hMap, FILE_MAP_READ, 0, 0, 0);
    if (!file_data) { CloseHandle(hMap); CloseHandle(hFile); return 1; }

    /* Read the trailer: last 8 bytes = [zip_offset:u32][magic:u32] */
    zip_offset = *(DWORD *)(file_data + file_size - 8);
    magic      = *(DWORD *)(file_data + file_size - 4);

    if (magic != COIL_MAGIC || zip_offset >= file_size - 8) {
        UnmapViewOfFile(file_data);
        CloseHandle(hMap);
        CloseHandle(hFile);
        return 1;
    }

    /* Only extract if the target exe doesn't already exist */
    if (GetFileAttributesW(target_exe) == INVALID_FILE_ATTRIBUTES) {
        DWORD zip_size = file_size - 8 - zip_offset;
        ensure_directory(extract_dir);
        if (!extract_zip(file_data + zip_offset, zip_size, extract_dir)) {
            UnmapViewOfFile(file_data);
            CloseHandle(hMap);
            CloseHandle(hFile);
            return 1;
        }
    }

    UnmapViewOfFile(file_data);
    CloseHandle(hMap);
    CloseHandle(hFile);

    /* Launch the extracted exe */
    ZeroMemory(&si, sizeof(si));
    si.cb = sizeof(si);
    ZeroMemory(&pi, sizeof(pi));

    /* Build full command line: "target_exe" original_args */
    WCHAR full_cmd[32768];
    wsprintfW(full_cmd, L"\"%s\" %s", target_exe, cmdLine ? cmdLine : L"");

    if (CreateProcessW(NULL, full_cmd, NULL, NULL, FALSE, 0,
                       NULL, NULL, &si, &pi)) {
        WaitForSingleObject(pi.hProcess, INFINITE);
        GetExitCodeProcess(pi.hProcess, &exit_code);
        CloseHandle(pi.hProcess);
        CloseHandle(pi.hThread);
    }

    return (int)exit_code;
}

/* Recursively create directories */
static BOOL ensure_directory(const WCHAR *path) {
    WCHAR tmp[MAX_PATH];
    WCHAR *p;

    wcscpy_s(tmp, MAX_PATH, path);
    for (p = tmp + 3; *p; p++) {   /* skip drive letter "C:\" */
        if (*p == L'\\' || *p == L'/') {
            *p = 0;
            CreateDirectoryW(tmp, NULL);
            *p = L'\\';
        }
    }
    CreateDirectoryW(tmp, NULL);
    return TRUE;
}

/* Extract a stored (uncompressed) zip to dest directory */
static BOOL extract_zip(const BYTE *data, DWORD size, const WCHAR *dest) {
    DWORD offset = 0;

    while (offset + sizeof(ZipLocalHeader) <= size) {
        const ZipLocalHeader *hdr = (const ZipLocalHeader *)(data + offset);

        if (hdr->signature != ZIP_LOCAL_SIG)
            break;

        DWORD header_end = offset + sizeof(ZipLocalHeader);
        if (header_end + hdr->name_len + hdr->extra_len > size)
            break;

        /* Get filename */
        const char *name_raw = (const char *)(data + header_end);
        WCHAR name_wide[MAX_PATH];
        ansi_to_wide(name_raw, name_wide, hdr->name_len);
        name_wide[hdr->name_len] = 0;

        /* Convert forward slashes */
        for (WCHAR *c = name_wide; *c; c++)
            if (*c == L'/') *c = L'\\';

        DWORD data_offset = header_end + hdr->name_len + hdr->extra_len;
        DWORD data_size = hdr->compressed_size;

        if (data_offset + data_size > size)
            break;

        /* Build full output path */
        WCHAR out_path[MAX_PATH];
        path_combine(out_path, dest, name_wide, MAX_PATH);

        /* If name ends with \ it's a directory */
        int name_end = (int)wcslen(name_wide) - 1;
        if (name_end >= 0 && name_wide[name_end] == L'\\') {
            ensure_directory(out_path);
        } else {
            /* Ensure parent directory exists */
            WCHAR parent[MAX_PATH];
            wcscpy_s(parent, MAX_PATH, out_path);
            WCHAR *last_slash = wcsrchr(parent, L'\\');
            if (last_slash) {
                *last_slash = 0;
                ensure_directory(parent);
            }

            /* Write the file */
            HANDLE hOut = CreateFileW(out_path, GENERIC_WRITE, 0, NULL,
                                      CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, NULL);
            if (hOut != INVALID_HANDLE_VALUE) {
                if (data_size > 0) {
                    DWORD written;
                    WriteFile(hOut, data + data_offset, data_size, &written, NULL);
                }
                CloseHandle(hOut);
            }
        }

        offset = data_offset + data_size;
    }

    return TRUE;
}

static void path_combine(WCHAR *out, const WCHAR *dir, const WCHAR *file, int max) {
    wsprintfW(out, L"%s\\%s", dir, file);
}

static void ansi_to_wide(const char *src, WCHAR *dst, int len) {
    for (int i = 0; i < len; i++)
        dst[i] = (WCHAR)(unsigned char)src[i];
}
