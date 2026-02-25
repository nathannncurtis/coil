/*
 * Coil portable bootloader v2.
 *
 * Minimal Windows executable that:
 * 1. Reads a zip archive appended after its own PE data
 * 2. Extracts to a hash-based cache: %LOCALAPPDATA%\coil\AppName\<hash>\
 * 3. Validates extraction via a .coil_ready marker file
 * 4. Uses a named mutex to prevent concurrent extraction races
 * 5. Falls back to TEMP or exe directory if LOCALAPPDATA is unavailable
 * 6. Launches the real application exe and forwards exit code
 * 7. Cleans up old cached builds (keeps last 3)
 *
 * Trailer format (last 12 bytes of file):
 *   [4 bytes: zip offset as uint32]
 *   [4 bytes: build hash as uint32]
 *   [4 bytes: magic 0x434F494C "COIL"]
 *
 * Build: zig cc -Os -target x86_64-windows-gnu -Wl,--subsystem,windows
 *        -Wl,--strip-all -municode -o bootloader.exe bootloader.c
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

#define ZIP_LOCAL_SIG   0x04034b50
#define COIL_MAGIC      0x434F494C  /* "COIL" */
#define COIL_MAX_CACHED 3
#define COIL_MARKER     L".coil_ready"

/* Forward declarations */
static BOOL ensure_directory(const WCHAR *path);
static BOOL extract_zip(const BYTE *data, DWORD size, const WCHAR *dest);
static void path_combine(WCHAR *out, const WCHAR *dir, const WCHAR *file, int max);
static void ansi_to_wide(const char *src, WCHAR *dst, int max);
static BOOL resolve_cache_dir(WCHAR *out, int max, const WCHAR *app_name, const WCHAR *hash_str);
static void cleanup_old_caches(const WCHAR *app_cache_dir, const WCHAR *current_hash);
static void rmdir_recursive(const WCHAR *path);

int WINAPI wWinMain(HINSTANCE hInst, HINSTANCE hPrev, LPWSTR cmdLine, int nShow) {
    WCHAR exe_path[MAX_PATH];
    WCHAR app_name[MAX_PATH];
    WCHAR extract_dir[MAX_PATH];
    WCHAR target_exe[MAX_PATH];
    WCHAR marker_path[MAX_PATH];
    WCHAR mutex_name[MAX_PATH];
    WCHAR app_cache_dir[MAX_PATH];
    WCHAR hash_str[16];
    WCHAR *slash, *dot;
    HANDLE hFile, hMap, hMutex;
    BYTE *file_data;
    DWORD file_size, zip_offset, build_hash, magic;
    DWORD exit_code = 1;
    BOOL need_extract;
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

    /* Memory-map our own exe */
    hFile = CreateFileW(exe_path, GENERIC_READ, FILE_SHARE_READ, NULL,
                        OPEN_EXISTING, 0, NULL);
    if (hFile == INVALID_HANDLE_VALUE) return 1;

    file_size = GetFileSize(hFile, NULL);
    if (file_size < 12) { CloseHandle(hFile); return 1; }

    hMap = CreateFileMappingW(hFile, NULL, PAGE_READONLY, 0, 0, NULL);
    if (!hMap) { CloseHandle(hFile); return 1; }

    file_data = (BYTE *)MapViewOfFile(hMap, FILE_MAP_READ, 0, 0, 0);
    if (!file_data) { CloseHandle(hMap); CloseHandle(hFile); return 1; }

    /* Read 12-byte trailer: [zip_offset:u32][build_hash:u32][magic:u32] */
    zip_offset = *(DWORD *)(file_data + file_size - 12);
    build_hash = *(DWORD *)(file_data + file_size - 8);
    magic      = *(DWORD *)(file_data + file_size - 4);

    if (magic != COIL_MAGIC || zip_offset >= file_size - 12) {
        UnmapViewOfFile(file_data);
        CloseHandle(hMap);
        CloseHandle(hFile);
        return 1;
    }

    /* Format build hash as 8-char hex string */
    wsprintfW(hash_str, L"%08x", build_hash);

    /* Resolve cache directory (tries LOCALAPPDATA, TEMP, then exe dir) */
    if (!resolve_cache_dir(extract_dir, MAX_PATH, app_name, hash_str)) {
        UnmapViewOfFile(file_data);
        CloseHandle(hMap);
        CloseHandle(hFile);
        return 1;
    }

    /* Build app cache parent dir path (for cleanup) */
    wcscpy_s(app_cache_dir, MAX_PATH, extract_dir);
    slash = wcsrchr(app_cache_dir, L'\\');
    if (slash) *slash = 0;

    /* Build target exe and marker paths */
    wsprintfW(target_exe, L"%s\\%s.exe", extract_dir, app_name);
    wsprintfW(marker_path, L"%s\\" COIL_MARKER, extract_dir);

    /* Check if extraction is needed — marker must exist and be valid */
    need_extract = (GetFileAttributesW(marker_path) == INVALID_FILE_ATTRIBUTES);

    if (need_extract) {
        /* If partial extraction exists (no marker), clean it up first */
        if (GetFileAttributesW(extract_dir) != INVALID_FILE_ATTRIBUTES) {
            rmdir_recursive(extract_dir);
        }

        /* Acquire named mutex to prevent concurrent extraction */
        wsprintfW(mutex_name, L"Global\\CoilExtract_%s_%s", app_name, hash_str);
        hMutex = CreateMutexW(NULL, FALSE, mutex_name);
        if (hMutex) {
            DWORD wait = WaitForSingleObject(hMutex, 60000); /* 60s timeout */
            if (wait == WAIT_OBJECT_0 || wait == WAIT_ABANDONED) {
                /* Re-check after acquiring lock — another instance may have extracted */
                if (GetFileAttributesW(marker_path) == INVALID_FILE_ATTRIBUTES) {
                    DWORD zip_size = file_size - 12 - zip_offset;
                    ensure_directory(extract_dir);

                    if (extract_zip(file_data + zip_offset, zip_size, extract_dir)) {
                        /* Write marker to signal extraction is complete */
                        HANDLE hMarker = CreateFileW(marker_path, GENERIC_WRITE, 0,
                                                     NULL, CREATE_ALWAYS,
                                                     FILE_ATTRIBUTE_HIDDEN, NULL);
                        if (hMarker != INVALID_HANDLE_VALUE) {
                            DWORD written;
                            WriteFile(hMarker, &build_hash, sizeof(build_hash),
                                      &written, NULL);
                            CloseHandle(hMarker);
                        }
                    } else {
                        /* Extraction failed — clean up partial state */
                        rmdir_recursive(extract_dir);
                        ReleaseMutex(hMutex);
                        CloseHandle(hMutex);
                        UnmapViewOfFile(file_data);
                        CloseHandle(hMap);
                        CloseHandle(hFile);
                        return 1;
                    }
                }
                ReleaseMutex(hMutex);
            }
            CloseHandle(hMutex);
        }
    }

    UnmapViewOfFile(file_data);
    CloseHandle(hMap);
    CloseHandle(hFile);

    /* Final check: refuse to launch if target exe doesn't exist */
    if (GetFileAttributesW(target_exe) == INVALID_FILE_ATTRIBUTES)
        return 1;

    /* Launch the extracted exe */
    ZeroMemory(&si, sizeof(si));
    si.cb = sizeof(si);
    ZeroMemory(&pi, sizeof(pi));

    {
        WCHAR full_cmd[32768];
        wsprintfW(full_cmd, L"\"%s\" %s", target_exe, cmdLine ? cmdLine : L"");

        if (CreateProcessW(NULL, full_cmd, NULL, NULL, FALSE, 0,
                           NULL, NULL, &si, &pi)) {
            WaitForSingleObject(pi.hProcess, INFINITE);
            GetExitCodeProcess(pi.hProcess, &exit_code);
            CloseHandle(pi.hProcess);
            CloseHandle(pi.hThread);
        }
    }

    /* Clean up old cached builds (best-effort, don't delay exit) */
    cleanup_old_caches(app_cache_dir, hash_str);

    return (int)exit_code;
}

/* Try to resolve a writable cache directory.
 * Order: %LOCALAPPDATA%\coil\AppName\hash
 *        %TEMP%\coil\AppName\hash
 *        <exe_dir>\.coil_cache\AppName\hash
 */
static BOOL resolve_cache_dir(WCHAR *out, int max,
                              const WCHAR *app_name, const WCHAR *hash_str) {
    WCHAR base[MAX_PATH];
    WCHAR test[MAX_PATH];

    /* Try LOCALAPPDATA */
    if (GetEnvironmentVariableW(L"LOCALAPPDATA", base, MAX_PATH)) {
        wsprintfW(test, L"%s\\coil", base);
        if (ensure_directory(test)) {
            if (GetFileAttributesW(test) != INVALID_FILE_ATTRIBUTES) {
                wsprintfW(out, L"%s\\%s\\%s", test, app_name, hash_str);
                return TRUE;
            }
        }
    }

    /* Try TEMP */
    if (GetTempPathW(MAX_PATH, base)) {
        wsprintfW(test, L"%s\\coil", base);
        /* Remove trailing backslash from GetTempPath */
        int len = (int)wcslen(test);
        if (len > 0 && test[len-1] == L'\\') test[len-1] = 0;
        wsprintfW(test, L"%scoil", base);
        if (ensure_directory(test)) {
            if (GetFileAttributesW(test) != INVALID_FILE_ATTRIBUTES) {
                wsprintfW(out, L"%s\\%s\\%s", test, app_name, hash_str);
                return TRUE;
            }
        }
    }

    /* Last resort: exe directory */
    {
        WCHAR exe_path[MAX_PATH];
        GetModuleFileNameW(NULL, exe_path, MAX_PATH);
        WCHAR *last = wcsrchr(exe_path, L'\\');
        if (last) *last = 0;
        wsprintfW(out, L"%s\\.coil_cache\\%s\\%s", exe_path, app_name, hash_str);
        return TRUE;
    }
}

/* Clean up old cache entries, keeping the newest COIL_MAX_CACHED. */
static void cleanup_old_caches(const WCHAR *app_cache_dir,
                               const WCHAR *current_hash) {
    WIN32_FIND_DATAW fd;
    WCHAR search[MAX_PATH];
    /* Track up to 32 cache entries */
    WCHAR entries[32][MAX_PATH];
    FILETIME times[32];
    int count = 0;
    int i, j, to_delete;

    wsprintfW(search, L"%s\\*", app_cache_dir);
    HANDLE hFind = FindFirstFileW(search, &fd);
    if (hFind == INVALID_HANDLE_VALUE) return;

    do {
        if (!(fd.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY)) continue;
        if (fd.cFileName[0] == L'.') continue;

        if (count < 32) {
            wcscpy_s(entries[count], MAX_PATH, fd.cFileName);
            times[count] = fd.ftCreationTime;
            count++;
        }
    } while (FindNextFileW(hFind, &fd));
    FindClose(hFind);

    if (count <= COIL_MAX_CACHED) return;

    /* Sort by creation time (oldest first) */
    for (i = 0; i < count - 1; i++) {
        for (j = i + 1; j < count; j++) {
            if (CompareFileTime(&times[i], &times[j]) > 0) {
                WCHAR tmp_name[MAX_PATH];
                FILETIME tmp_time;
                wcscpy_s(tmp_name, MAX_PATH, entries[i]);
                wcscpy_s(entries[i], MAX_PATH, entries[j]);
                wcscpy_s(entries[j], MAX_PATH, tmp_name);
                tmp_time = times[i];
                times[i] = times[j];
                times[j] = tmp_time;
            }
        }
    }

    /* Delete oldest, keep COIL_MAX_CACHED newest */
    to_delete = count - COIL_MAX_CACHED;
    for (i = 0; i < to_delete; i++) {
        /* Never delete the currently active cache */
        if (wcscmp(entries[i], current_hash) == 0) continue;

        WCHAR full_path[MAX_PATH];
        wsprintfW(full_path, L"%s\\%s", app_cache_dir, entries[i]);
        rmdir_recursive(full_path);
    }
}

/* Recursively delete a directory and all contents. */
static void rmdir_recursive(const WCHAR *path) {
    WIN32_FIND_DATAW fd;
    WCHAR search[MAX_PATH];
    WCHAR child[MAX_PATH];

    wsprintfW(search, L"%s\\*", path);
    HANDLE hFind = FindFirstFileW(search, &fd);
    if (hFind == INVALID_HANDLE_VALUE) {
        RemoveDirectoryW(path);
        return;
    }

    do {
        if (fd.cFileName[0] == L'.' &&
            (fd.cFileName[1] == 0 ||
             (fd.cFileName[1] == L'.' && fd.cFileName[2] == 0)))
            continue;

        wsprintfW(child, L"%s\\%s", path, fd.cFileName);
        if (fd.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY) {
            rmdir_recursive(child);
        } else {
            SetFileAttributesW(child, FILE_ATTRIBUTE_NORMAL);
            DeleteFileW(child);
        }
    } while (FindNextFileW(hFind, &fd));
    FindClose(hFind);

    RemoveDirectoryW(path);
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
