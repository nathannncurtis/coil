/* Coil portable bootloader. Rebuild with scripts/build_bootloader.py.
 * Retains the 12-byte trailer; SHA-256 of the archive identifies caches.
 * Authenticode certificates may follow the trailer and alignment padding.
 */
#define WIN32_LEAN_AND_MEAN
#define UNICODE
#define _UNICODE
#define TINFL_IMPLEMENTATION
#include <windows.h>
#include <bcrypt.h>
#include <stdio.h>
#include <stdlib.h>
#include <wchar.h>
#include <wctype.h>
#include "tinfl.h"
#define CAP 32768
#define MAGIC 0x434f494c
#define KEEP 3
#pragma pack(push, 1)
typedef struct {
    DWORD signature;
    WORD version, flags, compression, mod_time, mod_date;
    DWORD crc32, compressed_size, uncompressed_size;
    WORD name_len, extra_len;
} ZipHeader;
#pragma pack(pop)
static DWORD crc_table[256];
static const WCHAR *failure = L"The portable application could not start.";
static WCHAR *buffer(void) { return calloc(CAP, sizeof(WCHAR)); }
static BOOL join(WCHAR *out, const WCHAR *dir, const WCHAR *name) {
    return swprintf_s(out, CAP, L"%s\\%s", dir, name) > 0;
}
static BOOL directory(const WCHAR *path) {
    DWORD a = GetFileAttributesW(path);
    return a != INVALID_FILE_ATTRIBUTES && (a & FILE_ATTRIBUTE_DIRECTORY) && !(a & FILE_ATTRIBUTE_REPARSE_POINT);
}
static BOOL regular(const WCHAR *path) {
    DWORD a = GetFileAttributesW(path);
    return a != INVALID_FILE_ATTRIBUTES && !(a & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT));
}
/* Refuse junctions/symlinks in every directory component. */
static BOOL mkdirs(const WCHAR *path) {
    BOOL ok = FALSE;
    WCHAR *copy = buffer(), *p;
    if (!copy || wcscpy_s(copy, CAP, path)) goto done;
    if (!wcsncmp(copy, L"\\\\?\\UNC\\", 8)) {
        p = wcschr(copy + 8, L'\\');
        if (!p || !(p = wcschr(p + 1, L'\\'))) goto done;
        p++;
    } else if (!wcsncmp(copy, L"\\\\?\\", 4)) p = copy + 7;
    else if (copy[1] == L':') p = copy + 3;
    else goto done;
    for (; *p; p++) {
        if (*p != L'\\') continue;
        *p = 0;
        if (!CreateDirectoryW(copy, NULL) && !directory(copy)) goto done;
        *p = L'\\';
    }
    ok = CreateDirectoryW(copy, NULL) || directory(copy);
done:
    free(copy); return ok;
}
static BOOL extended(WCHAR *out, const WCHAR *path) {
    WCHAR *full = buffer(); DWORD n; BOOL ok = FALSE;
    if (!full) return FALSE;
    n = GetFullPathNameW(path, CAP, full, NULL);
    if (!n || n >= CAP) goto done;
    if (!wcsncmp(full, L"\\\\?\\", 4)) ok = wcscpy_s(out, CAP, full) == 0;
    else if (!wcsncmp(full, L"\\\\", 2)) ok = swprintf_s(out, CAP, L"\\\\?\\UNC\\%s", full + 2) > 0;
    else ok = swprintf_s(out, CAP, L"\\\\?\\%s", full) > 0;
done:
    free(full); return ok;
}
static void remove_tree(const WCHAR *path) {
    WIN32_FIND_DATAW fd; HANDLE find;
    WCHAR *search = buffer(), *child = buffer();
    DWORD a = GetFileAttributesW(path);
    if (!search || !child || a == INVALID_FILE_ATTRIBUTES) goto done;
    if (a & FILE_ATTRIBUTE_REPARSE_POINT) {
        if (a & FILE_ATTRIBUTE_DIRECTORY) RemoveDirectoryW(path); else DeleteFileW(path);
        goto done;
    }
    if (!join(search, path, L"*")) goto done;
    find = FindFirstFileW(search, &fd);
    if (find != INVALID_HANDLE_VALUE) {
        do {
            if (!wcscmp(fd.cFileName, L".") || !wcscmp(fd.cFileName, L"..")) continue;
            if (!join(child, path, fd.cFileName)) continue;
            if (fd.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY) remove_tree(child);
            else {
                if (!(fd.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT)) SetFileAttributesW(child, FILE_ATTRIBUTE_NORMAL);
                DeleteFileW(child);
            }
        } while (FindNextFileW(find, &fd));
        FindClose(find);
    }
    RemoveDirectoryW(path);
done:
    free(search); free(child);
}
static void init_crc(void) {
    DWORD i, j, c;
    for (i = 0; i < 256; i++) {
        c = i;
        for (j = 0; j < 8; j++) c = (c >> 1) ^ ((c & 1) ? 0xedb88320 : 0);
        crc_table[i] = c;
    }
}
static DWORD crc_update(DWORD c, const BYTE *data, DWORD size) {
    DWORD i;
    for (i = 0; i < size; i++) c = crc_table[(c ^ data[i]) & 255] ^ (c >> 8);
    return c;
}
static BOOL sha256(const BYTE *data, DWORD size, BYTE digest[32]) {
    BCRYPT_ALG_HANDLE alg = NULL; BCRYPT_HASH_HANDLE hash = NULL; BOOL ok = FALSE;
    if (BCryptOpenAlgorithmProvider(&alg, BCRYPT_SHA256_ALGORITHM, NULL, 0) < 0) return FALSE;
    if (BCryptCreateHash(alg, &hash, NULL, 0, NULL, 0, 0) >= 0) {
        ok = BCryptHashData(hash, (PUCHAR)data, size, 0) >= 0 && BCryptFinishHash(hash, digest, 32, 0) >= 0;
        BCryptDestroyHash(hash);
    }
    BCryptCloseAlgorithmProvider(alg, 0); return ok;
}
static BOOL filename(const ZipHeader *h, const BYTE *name, WCHAR *wide) {
    int n; WCHAR *segment, *p;
    if (!h->name_len || memchr(name, 0, h->name_len)) return FALSE;
    n = MultiByteToWideChar((h->flags & 0x800) ? CP_UTF8 : 437,
        (h->flags & 0x800) ? MB_ERR_INVALID_CHARS : 0,
        (const char *)name, h->name_len, wide, CAP - 1);
    if (!n) return FALSE;
    wide[n] = 0;
    for (p = wide; *p; p++) if (*p == L'/') *p = L'\\';
    if (wide[0] == L'\\') return FALSE;
    segment = wide;
    for (p = wide;; p++) {
        if (*p == L':' || *p == L'"' || *p == L'<' || *p == L'>' || *p == L'|' || *p == L'?' || *p == L'*' || (*p && *p < 32)) return FALSE;
        if (*p == L'\\' || !*p) {
            size_t len = (size_t)(p - segment);
            if (!len || len > 255 || segment[len - 1] == L'.' || segment[len - 1] == L' ') return FALSE;
            if (!*p || !p[1]) break;
            segment = p + 1;
        }
    }
    return TRUE;
}
static BOOL file_matches(const WCHAR *path, const ZipHeader *h) {
    HANDLE f; BYTE buf[65536]; DWORD read, crc = 0xffffffff;
    LARGE_INTEGER size; BOOL ok = FALSE;
    if (!regular(path)) return FALSE;
    f = CreateFileW(path, GENERIC_READ, FILE_SHARE_READ, NULL, OPEN_EXISTING, FILE_FLAG_OPEN_REPARSE_POINT, NULL);
    if (f == INVALID_HANDLE_VALUE) return FALSE;
    if (!GetFileSizeEx(f, &size) || size.QuadPart != h->uncompressed_size) goto done;
    for (;;) {
        if (!ReadFile(f, buf, sizeof(buf), &read, NULL)) goto done;
        if (!read) break;
        crc = crc_update(crc, buf, read);
    }
    ok = (crc ^ 0xffffffff) == h->crc32;
done:
    CloseHandle(f); return ok;
}
/* Application assets may be deliberately mutable. Validate/repair executable
 * runtime files, but preserve existing data files and user-created files. */
static BOOL runtime_file(const WCHAR *name) {
    const WCHAR *suffix = wcsrchr(name, L'.');
    if (!suffix) return FALSE;
    if (!wcschr(name, L'\\')) return !_wcsicmp(suffix, L".exe") || !_wcsicmp(suffix, L".dll");
    if (wcsncmp(name, L"_internal\\", 10)) return FALSE;
    return !_wcsicmp(suffix, L".py") || !_wcsicmp(suffix, L".pyc") || !_wcsicmp(suffix, L".pyd")
        || !_wcsicmp(suffix, L".dll") || !_wcsicmp(suffix, L".zip");
}
/* Validate local records, names, data sizes, inflate, CRCs and writes.
 * check also verifies cached payload files on every launch. */
static BOOL walk_zip(const BYTE *data, DWORD size, const WCHAR *dest, BOOL check, WCHAR *entry) {
    DWORD offset = 0, count = 0; BOOL ok = FALSE;
    WCHAR *name = buffer(), *out = buffer(), *parent = buffer();
    if (!name || !out || !parent) goto done;
    if (entry) entry[0] = 0;
    while (offset <= size && size - offset >= sizeof(ZipHeader)) {
        const ZipHeader *h = (const ZipHeader *)(data + offset);
        DWORD start; size_t len;
        if (h->signature == 0x02014b50) { ok = count > 0; goto done; }
        if (h->signature != 0x04034b50 || (h->flags & (1 | 8)) || (h->compression != 0 && h->compression != 8)) goto done;
        start = offset + sizeof(ZipHeader);
        if (h->name_len > size - start || !filename(h, data + start, name)) goto done;
        start += h->name_len;
        if (h->extra_len > size - start) goto done;
        start += h->extra_len;
        if (h->compressed_size > size - start) goto done;
        len = wcslen(name);
        if (entry && !wcsncmp(name, L"_internal\\_boot_", 16) && len > 19
                && !wcscmp(name + len - 3, L".py") && !wcschr(name + 16, L'\\')) {
            if (entry[0]) goto done;
            if (swprintf_s(entry, CAP, L"%.*s.exe", (int)len - 19, name + 16) < 0) goto done;
        }
        if (!dest) { offset = start + h->compressed_size; count++; continue; }
        if (!join(out, dest, name)) goto done;
        if (name[len - 1] == L'\\') {
            out[wcslen(out) - 1] = 0;
            if (!(check ? directory(out) : mkdirs(out))) goto done;
        } else if (check) {
            WCHAR *slash;
            wcscpy_s(parent, CAP, out);
            slash = wcsrchr(parent, L'\\'); if (!slash) goto done; *slash = 0;
            if (!mkdirs(parent) || !(runtime_file(name) ? file_matches(out, h) : regular(out))) goto done;
        } else {
            WCHAR *slash; BYTE *inflated = NULL;
            const BYTE *contents = data + start;
            HANDLE f; DWORD written = 0; BOOL wrote;
            wcscpy_s(parent, CAP, out);
            slash = wcsrchr(parent, L'\\'); if (!slash) goto done; *slash = 0;
            if (!mkdirs(parent)) goto done;
            if (regular(out) && (!runtime_file(name) || file_matches(out, h))) {
                offset = start + h->compressed_size; count++; continue;
            }
            if (GetFileAttributesW(out) != INVALID_FILE_ATTRIBUTES && !regular(out)) goto done;
            if (h->compression == 8) {
                size_t result;
                inflated = VirtualAlloc(NULL, h->uncompressed_size ? h->uncompressed_size : 1, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE);
                if (!inflated) goto done;
                result = tinfl_decompress_mem_to_mem(inflated, h->uncompressed_size, data + start, h->compressed_size, 0);
                if (result == TINFL_DECOMPRESS_MEM_TO_MEM_FAILED || result != h->uncompressed_size) {
                    VirtualFree(inflated, 0, MEM_RELEASE); goto done;
                }
                contents = inflated;
            } else if (h->compressed_size != h->uncompressed_size) goto done;
            if ((crc_update(0xffffffff, contents, h->uncompressed_size) ^ 0xffffffff) != h->crc32) {
                if (inflated) VirtualFree(inflated, 0, MEM_RELEASE);
                goto done;
            }
            f = CreateFileW(out, GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT, NULL);
            wrote = f != INVALID_HANDLE_VALUE && WriteFile(f, contents, h->uncompressed_size, &written, NULL) && written == h->uncompressed_size;
            if (f != INVALID_HANDLE_VALUE) CloseHandle(f);
            if (inflated) VirtualFree(inflated, 0, MEM_RELEASE);
            if (!wrote) goto done;
        }
        offset = start + h->compressed_size; count++;
    }
done:
    free(name); free(out); free(parent); return ok;
}
static BOOL trailer(const BYTE *data, DWORD size, DWORD *offset, DWORD *length) {
    DWORD end = size, cert, cert_size, pad;
    const IMAGE_DOS_HEADER *dos; const IMAGE_NT_HEADERS64 *nt;
    if (size < sizeof(IMAGE_NT_HEADERS64)) return FALSE;
    dos = (const IMAGE_DOS_HEADER *)data;
    if (dos->e_magic != IMAGE_DOS_SIGNATURE || dos->e_lfanew < 0 || (DWORD)dos->e_lfanew > size - sizeof(IMAGE_NT_HEADERS64)) return FALSE;
    nt = (const IMAGE_NT_HEADERS64 *)(data + dos->e_lfanew);
    if (nt->Signature != IMAGE_NT_SIGNATURE || nt->OptionalHeader.Magic != IMAGE_NT_OPTIONAL_HDR64_MAGIC) return FALSE;
    cert = nt->OptionalHeader.DataDirectory[IMAGE_DIRECTORY_ENTRY_SECURITY].VirtualAddress;
    cert_size = nt->OptionalHeader.DataDirectory[IMAGE_DIRECTORY_ENTRY_SECURITY].Size;
    if (cert || cert_size) {
        if (!cert || cert > size || cert_size > size - cert || cert + cert_size != size) return FALSE;
        end = cert;
    }
    for (pad = 0; pad <= (cert ? 7u : 0u); pad++) {
        DWORD at;
        if (end < pad + 12) return FALSE;
        at = end - pad - 12;
        if (*(const DWORD *)(data + at + 8) == MAGIC) {
            *offset = *(const DWORD *)(data + at);
            if (*offset >= at) return FALSE;
            *length = at - *offset; return TRUE;
        }
        if (data[end - pad - 1] != 0) break;
    }
    return FALSE;
}
static BOOL resolve_cache(WCHAR *out, const WCHAR *app, const WCHAR *hash, const WCHAR *exe) {
    WCHAR *base = buffer(), *absolute = buffer(), *test = buffer();
    BOOL ok = FALSE; int attempt;
    if (!base || !absolute || !test) goto done;
    for (attempt = 0; attempt < 3; attempt++) {
        DWORD n; HANDLE f;
        if (attempt == 0) n = GetEnvironmentVariableW(L"LOCALAPPDATA", base, CAP);
        else if (attempt == 1) n = GetTempPathW(CAP, base);
        else {
            WCHAR *slash;
            wcscpy_s(base, CAP, exe);
            slash = wcsrchr(base, L'\\'); if (!slash) continue; *slash = 0;
            n = (DWORD)wcslen(base);
        }
        if (!n || n >= CAP || !extended(absolute, base)) continue;
        if (swprintf_s(out, CAP, L"%s\\%s\\%s", absolute, attempt == 2 ? L".coil_cache" : L"coil", app) < 0 || !mkdirs(out)) continue;
        if (swprintf_s(test, CAP, L"%s\\.write-test-%lu", out, GetCurrentProcessId()) < 0) continue;
        f = CreateFileW(test, GENERIC_WRITE, 0, NULL, CREATE_NEW, FILE_ATTRIBUTE_TEMPORARY | FILE_FLAG_DELETE_ON_CLOSE, NULL);
        if (f == INVALID_HANDLE_VALUE) continue;
        CloseHandle(f);
        if (!join(test, out, hash) || wcscpy_s(out, CAP, test)) continue;
        ok = TRUE; break;
    }
done:
    free(base); free(absolute); free(test); return ok;
}
static BOOL marker_matches(const WCHAR *path, const BYTE digest[32]) {
    HANDLE f; BYTE actual[33]; DWORD read; BOOL ok;
    if (!regular(path)) return FALSE;
    f = CreateFileW(path, GENERIC_READ, FILE_SHARE_READ, NULL, OPEN_EXISTING, FILE_FLAG_OPEN_REPARSE_POINT, NULL);
    if (f == INVALID_HANDLE_VALUE) return FALSE;
    ok = ReadFile(f, actual, sizeof(actual), &read, NULL) && read == 32 && !memcmp(actual, digest, 32);
    CloseHandle(f); return ok;
}
static HANDLE lease_file(const WCHAR *dir, DWORD share) {
    WCHAR *path = buffer(); HANDLE f = INVALID_HANDLE_VALUE;
    if (path && join(path, dir, L".coil_running")) f = CreateFileW(path, GENERIC_READ, share, NULL, OPEN_ALWAYS, FILE_ATTRIBUTE_HIDDEN | FILE_FLAG_OPEN_REPARSE_POINT, NULL);
    free(path); return f;
}
/* App file lock covers extraction, lease acquisition and cleanup; every live
 * instance holds a shared lease until its child exits. */
typedef struct { WCHAR name[65]; FILETIME time; } CacheEntry;
static int compare_cache(const void *a, const void *b) {
    return CompareFileTime(&((const CacheEntry *)a)->time, &((const CacheEntry *)b)->time);
}
static void cleanup(const WCHAR *parent, const WCHAR *current) {
    WIN32_FIND_DATAW fd; HANDLE find;
    WCHAR *search = buffer(), *path = buffer();
    CacheEntry *entries = NULL; size_t count = 0, i, remove_count;
    if (!search || !path || !join(search, parent, L"*")) goto done;
    find = FindFirstFileW(search, &fd);
    if (find == INVALID_HANDLE_VALUE) goto done;
    do {
        size_t n = wcslen(fd.cFileName); CacheEntry *grown;
        if (!(fd.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY) || (fd.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT)) continue;
        /* Legacy caches have no leases: leave them for explicit cache clear. */
        if (n != 64) continue;
        for (i = 0; i < n; i++) if (!iswxdigit(fd.cFileName[i])) break;
        if (i != n) continue;
        grown = realloc(entries, (count + 1) * sizeof(CacheEntry)); if (!grown) break;
        entries = grown; wcscpy_s(entries[count].name, 65, fd.cFileName);
        entries[count++].time = fd.ftCreationTime;
    } while (FindNextFileW(find, &fd));
    FindClose(find);
    if (count <= KEEP) goto done;
    qsort(entries, count, sizeof(CacheEntry), compare_cache);
    remove_count = count - KEEP;
    for (i = 0; i < count && remove_count; i++) {
        HANDLE lease;
        if (!wcscmp(entries[i].name, current) || !join(path, parent, entries[i].name)) continue;
        lease = lease_file(path, 0); if (lease == INVALID_HANDLE_VALUE) continue;
        CloseHandle(lease); remove_tree(path); remove_count--;
    }
done:
    free(entries); free(search); free(path);
}
static void report_failure(void) {
    HANDLE err = GetStdHandle(STD_ERROR_HANDLE);
    WCHAR message[512]; char utf8[1600]; DWORD written; int n;
    swprintf_s(message, 512, L"Coil: %s (Windows error %lu)\r\n", failure, GetLastError());
    n = WideCharToMultiByte(CP_UTF8, 0, message, -1, utf8, sizeof(utf8), NULL, NULL);
    if (err && err != INVALID_HANDLE_VALUE && n > 0 && WriteFile(err, utf8, n - 1, &written, NULL)) return;
    OutputDebugStringW(message);
}
int WINAPI wWinMain(HINSTANCE instance, HINSTANCE previous, LPWSTR cmdline, int show) {
    WCHAR *exe = buffer(), *mapped = buffer(), *entry = buffer(), *cache = buffer();
    WCHAR *target = buffer(), *marker = buffer(), *parent = buffer(), *command = buffer();
    WCHAR app[256], hash[65], pid[16], *slash;
    HANDLE file = INVALID_HANDLE_VALUE, mapping = NULL, mutex = NULL, lease = INVALID_HANDLE_VALUE;
    BYTE *data = NULL, digest[32];
    DWORD offset, length, exit_code = 1, n, i, inherited = 0;
    LARGE_INTEGER size; BOOL launched = FALSE, attr_ready = FALSE;
    STARTUPINFOEXW si; PROCESS_INFORMATION pi;
    HANDLE std_handles[3] = {NULL, NULL, NULL}, inherited_handles[4]; SIZE_T attr_size = 0;
    (void)instance; (void)previous; (void)show;
    ZeroMemory(&si, sizeof(si)); ZeroMemory(&pi, sizeof(pi));
    if (!exe || !mapped || !entry || !cache || !target || !marker || !parent || !command) goto done;
    n = GetModuleFileNameW(NULL, exe, CAP);
    if (!n || n >= CAP || !extended(mapped, exe)) goto done;
    file = CreateFileW(mapped, GENERIC_READ, FILE_SHARE_READ, NULL, OPEN_EXISTING, 0, NULL);
    if (file == INVALID_HANDLE_VALUE || !GetFileSizeEx(file, &size) || size.QuadPart < 12 || size.QuadPart > MAXDWORD) goto done;
    mapping = CreateFileMappingW(file, NULL, PAGE_READONLY, 0, 0, NULL);
    if (!mapping || !(data = MapViewOfFile(mapping, FILE_MAP_READ, 0, 0, 0))) goto done;
    failure = L"The portable archive is corrupt or uses an unsupported ZIP format. Rebuild or replace this executable.";
    init_crc();
    if (!trailer(data, (DWORD)size.QuadPart, &offset, &length) || !walk_zip(data + offset, length, NULL, FALSE, entry) || !entry[0] || !sha256(data + offset, length, digest)) goto done;
    if (wcslen(entry) >= 256) goto done;
    wcscpy_s(app, 256, entry); app[wcslen(app) - 4] = 0;
    for (i = 0; i < 32; i++) swprintf_s(hash + i * 2, 65 - i * 2, L"%02x", digest[i]);
    failure = L"Cannot create or repair the portable runtime cache.";
    if (!resolve_cache(cache, app, hash, exe) || !join(target, cache, entry) || !join(marker, cache, L".coil_ready")) goto done;
    wcscpy_s(parent, CAP, cache); slash = wcsrchr(parent, L'\\'); if (!slash) goto done; *slash = 0;
    /* File sharing locks also cover case aliases and different logon sessions. */
    if (!join(command, parent, L".coil_lock")) goto done;
    for (i = 0; i < 600; i++) {
        mutex = CreateFileW(command, GENERIC_READ, 0, NULL, OPEN_ALWAYS, FILE_ATTRIBUTE_HIDDEN | FILE_FLAG_OPEN_REPARSE_POINT, NULL);
        if (mutex != INVALID_HANDLE_VALUE) break;
        mutex = NULL;
        if (GetLastError() != ERROR_SHARING_VIOLATION) goto done;
        Sleep(100);
    }
    if (!mutex) goto done;
    if (!mkdirs(cache)) goto done;
    if (!marker_matches(marker, digest) || !walk_zip(data + offset, length, cache, TRUE, NULL)) {
        HANDLE exclusive = lease_file(cache, 0), ready; DWORD written;
        if (exclusive == INVALID_HANDLE_VALUE) {
            failure = L"The runtime cache is damaged and another instance is using it. Close other instances and retry.";
            goto done;
        }
        CloseHandle(exclusive);
        if (!walk_zip(data + offset, length, cache, FALSE, NULL)) goto done;
        ready = CreateFileW(marker, GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, FILE_ATTRIBUTE_HIDDEN, NULL);
        if (ready == INVALID_HANDLE_VALUE) goto done;
        n = WriteFile(ready, digest, 32, &written, NULL) && written == 32 && FlushFileBuffers(ready);
        CloseHandle(ready); if (!n) goto done;
    }
    lease = lease_file(cache, FILE_SHARE_READ); if (lease == INVALID_HANDLE_VALUE) goto done;
    CloseHandle(mutex); mutex = NULL;
    failure = L"Cannot launch the extracted application.";
    if (swprintf_s(command, CAP, L"\"%s\" %s", target, cmdline ? cmdline : L"") < 0) goto done;
    /* Portable-only inner launcher verifies this actual parent process. */
    swprintf_s(pid, 16, L"%lu", GetCurrentProcessId());
    SetEnvironmentVariableW(L"_COIL_PORTABLE_PARENT_PID", pid);
    SetEnvironmentVariableW(L"_COIL_PORTABLE_EXE", exe);
    si.StartupInfo.cb = sizeof(si);
    for (i = 0; i < 3; i++) {
        HANDLE original = GetStdHandle(i == 0 ? STD_INPUT_HANDLE : i == 1 ? STD_OUTPUT_HANDLE : STD_ERROR_HANDLE);
        if (original && original != INVALID_HANDLE_VALUE && DuplicateHandle(GetCurrentProcess(), original,
            GetCurrentProcess(), &std_handles[i], 0, TRUE, DUPLICATE_SAME_ACCESS)) inherited++;
    }
    {
        DWORD count = 0;
        if (!SetHandleInformation(lease, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT)) goto done;
        inherited_handles[count++] = lease;
        if (inherited) {
        for (i = 0; i < 3; i++) {
            if (!std_handles[i]) std_handles[i] = CreateFileW(L"NUL", i ? GENERIC_WRITE : GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_WRITE, NULL, OPEN_EXISTING, 0, NULL);
            if (!std_handles[i] || std_handles[i] == INVALID_HANDLE_VALUE || !SetHandleInformation(std_handles[i], HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT)) goto done;
            inherited_handles[count++] = std_handles[i];
        }
        si.StartupInfo.dwFlags |= STARTF_USESTDHANDLES;
        si.StartupInfo.hStdInput = std_handles[0]; si.StartupInfo.hStdOutput = std_handles[1]; si.StartupInfo.hStdError = std_handles[2];
        }
        InitializeProcThreadAttributeList(NULL, 1, 0, &attr_size);
        si.lpAttributeList = malloc(attr_size);
        if (!si.lpAttributeList || !InitializeProcThreadAttributeList(si.lpAttributeList, 1, 0, &attr_size)) goto done;
        attr_ready = TRUE;
        if (!UpdateProcThreadAttribute(si.lpAttributeList, 0, PROC_THREAD_ATTRIBUTE_HANDLE_LIST, inherited_handles, count * sizeof(HANDLE), NULL, NULL)) goto done;
    }
    if (CreateProcessW(target, command, NULL, NULL, TRUE, EXTENDED_STARTUPINFO_PRESENT, NULL, NULL, &si.StartupInfo, &pi)) {
        launched = TRUE; CloseHandle(pi.hThread);
        WaitForSingleObject(pi.hProcess, INFINITE); GetExitCodeProcess(pi.hProcess, &exit_code); CloseHandle(pi.hProcess);
    }
    if (join(command, parent, L".coil_lock")) {
        mutex = CreateFileW(command, GENERIC_READ, 0, NULL, OPEN_ALWAYS, FILE_ATTRIBUTE_HIDDEN | FILE_FLAG_OPEN_REPARSE_POINT, NULL);
        if (mutex != INVALID_HANDLE_VALUE) cleanup(parent, hash);
        else mutex = NULL;
    }
done:
    if (!launched) report_failure();
    if (attr_ready) DeleteProcThreadAttributeList(si.lpAttributeList);
    free(si.lpAttributeList);
    for (i = 0; i < 3; i++) if (std_handles[i] && std_handles[i] != INVALID_HANDLE_VALUE) CloseHandle(std_handles[i]);
    if (lease != INVALID_HANDLE_VALUE) CloseHandle(lease);
    if (mutex) CloseHandle(mutex);
    if (data) UnmapViewOfFile(data);
    if (mapping) CloseHandle(mapping);
    if (file != INVALID_HANDLE_VALUE) CloseHandle(file);
    free(exe); free(mapped); free(entry); free(cache); free(target); free(marker); free(parent); free(command);
    return (int)exit_code;
}
