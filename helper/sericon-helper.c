/* Sericon's optional Linux DUT helper.
 * One foreground invocation per request: no daemon, shell, UART ownership,
 * terminal changes. File writes require explicit upload commands. stdout is a checksummed hex
 * payload; the Rust host supplies the random request tag and shell boundary.
 */
#define _GNU_SOURCE
#define _FILE_OFFSET_BITS 64
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/statvfs.h>
#include <sys/utsname.h>
#include <sys/vfs.h>
#include <unistd.h>

#ifndef SC_ARCH
#define SC_ARCH "unknown"
#endif
#define LIMIT (64U * 1024U * 1024U)
#define CHUNK 1024

struct sum { uint32_t crc; uint64_t len; };
static struct sum output;
static const char hex[] = "0123456789abcdef";

static void fail(void) {
    static const char message[] = "sericon-helper: invalid request, unsupported path, or I/O failure\n";
    (void)!write(2, message, sizeof(message)-1);
    _exit(1);
}
static void write_all(const void *bytes, size_t size) {
    const unsigned char *p = bytes;
    while (size) {
        ssize_t n = write(1, p, size);
        if (n < 0 && errno == EINTR) continue;
        if (n <= 0) fail();
        p += n; size -= (size_t)n;
    }
}
static void text(const char *s) { write_all(s, strlen(s)); }
static void byte(struct sum *s, unsigned char b) {
    s->crc ^= (uint32_t)b << 24;
    for (int i=0; i<8; i++)
        s->crc = (s->crc << 1) ^ (s->crc & 0x80000000U ? 0x04c11db7U : 0);
}
static void update(struct sum *s, const unsigned char *p, size_t n) {
    s->len += n;
    while (n--) byte(s, *p++);
}
static uint32_t finish(struct sum s) {
    for (uint64_t n=s.len; n; n >>= 8) byte(&s, (unsigned char)n);
    return ~s.crc;
}
static char *decimal(char *end, uint64_t n) {
    *--end = 0;
    do { *--end = (char)('0' + n%10); n /= 10; } while (n);
    return end;
}
static void number(uint64_t n) {
    char s[32]; text(decimal(s+sizeof(s), n));
}
static void payload(const void *data, size_t n) {
    const unsigned char *p = data;
    char buf[CHUNK*2];
    update(&output, p, n);
    while (n) {
        size_t take = n > CHUNK ? CHUNK : n;
        for (size_t i=0; i<take; i++) {
            buf[2*i] = hex[p[i] >> 4]; buf[2*i+1] = hex[p[i] & 15];
        }
        write_all(buf, take*2); p += take; n -= take;
    }
}
static void field(const char *s) { payload(s, strlen(s)+1); }
static int prefix(const char *p, const char *base) {
    size_t n = strlen(base);
    return !strncmp(p, base, n) && (!p[n] || p[n]=='/');
}
static void valid_path(const char *path) {
    size_t n = strlen(path);
    if (!n || n > 512 || path[0] != '/') fail();
    for (size_t i=0; i<n; i++)
        if ((unsigned char)path[i]<32 || path[i]==127) fail();
}
static int regular(const char *path) {
    char resolved[4096]; struct stat st; struct statfs fs;
    valid_path(path);
    if (!realpath(path, resolved) || prefix(resolved,"/proc") ||
        prefix(resolved,"/sys") || prefix(resolved,"/dev")) fail();
    int fd = open(path, O_RDONLY|O_NOFOLLOW|O_NONBLOCK|O_CLOEXEC);
    if (fd < 0 || fstat(fd,&st) || !S_ISREG(st.st_mode) ||
        st.st_size < 0 || (uint64_t)st.st_size > LIMIT || fstatfs(fd,&fs)) fail();
    /* Catch proc/sys aliases and bind mounts as well as canonical paths. */
    if ((unsigned long)fs.f_type==0x9fa0UL ||
        (unsigned long)fs.f_type==0x62656572UL ||
        (unsigned long)fs.f_type==0x1373UL) fail();
    return fd;
}
static ssize_t read_retry(int fd, void *p, size_t n) {
    ssize_t got;
    do { got=read(fd,p,n); } while (got<0 && errno==EINTR);
    if (got<0) fail();
    return got;
}
static void checksum_file(const char *path) {
    int fd=regular(path); struct sum s={0,0}; unsigned char buf[CHUNK];
    ssize_t n;
    while ((n=read_retry(fd,buf,sizeof(buf)))>0) {
        if (s.len+(uint64_t)n>LIMIT) fail();
        update(&s,buf,(size_t)n);
    }
    if (close(fd)) fail();
    char crc[32], len[32];
    char *c=decimal(crc+sizeof(crc),finish(s));
    char *l=decimal(len+sizeof(len),s.len);
    payload(c,strlen(c)); payload(" ",1); payload(l,strlen(l));
}
static uint64_t parse_number(const char *s) {
    uint64_t n=0;
    if (!*s) fail();
    while (*s) {
        if (*s<'0' || *s>'9' || n>LIMIT) fail();
        n=n*10+(unsigned)(*s++-'0');
    }
    if (n>LIMIT) fail();
    return n;
}
static void read_chunk(const char *path, const char *offset) {
    int fd=regular(path); uint64_t start=parse_number(offset);
    if (start%CHUNK || lseek(fd,(off_t)start,SEEK_SET)!=(off_t)start) fail();
    unsigned char buf[CHUNK]; size_t got=0;
    while (got<sizeof(buf)) {
        ssize_t n=read_retry(fd,buf+got,sizeof(buf)-got);
        if (!n) break;
        got+=(size_t)n;
    }
    if (close(fd)) fail();
    payload(buf,got);
}
static void list(const char *path) {
    valid_path(path);
    DIR *dir=opendir(path);
    if (!dir) fail();
    unsigned count=0;
    for (;;) {
        errno=0;
        struct dirent *entry=readdir(dir);
        if (!entry) { if (errno) fail(); break; }
        if (!strcmp(entry->d_name,".") || !strcmp(entry->d_name,"..")) continue;
        if (++count>512) fail();
        struct stat st;
        if (fstatat(dirfd(dir),entry->d_name,&st,AT_SYMLINK_NOFOLLOW)) fail();
        const char *kind=S_ISLNK(st.st_mode)?"link":S_ISDIR(st.st_mode)?"directory":S_ISREG(st.st_mode)?"file":"other";
        field(kind);
        size_t n=strlen(path);
        payload(path,n);
        if (path[n-1]!='/') payload("/",1);
        field(entry->d_name);
    }
    if (closedir(dir)) fail();
}

static void reason(const char *message) {
    (void)!write(2,message,strlen(message));
    (void)!write(2,"\n",1);
    _exit(1);
}
static uint32_t crc_number(const char *s) {
    uint64_t n=0;
    if (!*s) fail();
    while (*s) {
        if (*s<'0' || *s>'9' || n>UINT32_MAX) fail();
        n=n*10+(unsigned)(*s++-'0');
    }
    if (n>UINT32_MAX) fail();
    return (uint32_t)n;
}
static void number_field(uint64_t n) {
    char s[32]; field(decimal(s+sizeof(s),n));
}
static int parent(const char *path, char leaf[513]) {
    char dir[513], resolved[4096]; struct statfs fs;
    valid_path(path);
    const char *slash=strrchr(path,'/');
    size_t n=(size_t)(slash-path);
    memcpy(dir,path,n); dir[n]=0;
    strcpy(leaf,slash+1);
    if (!*leaf || !strcmp(leaf,".") || !strcmp(leaf,"..")) fail();
    if (!realpath(n?dir:"/",resolved) || prefix(resolved,"/proc") ||
        prefix(resolved,"/sys") || prefix(resolved,"/dev")) fail();
    int fd=open(resolved,O_RDONLY|O_DIRECTORY|O_NOFOLLOW|O_CLOEXEC);
    if (fd<0 || fstatfs(fd,&fs) || (unsigned long)fs.f_type==0x9fa0UL ||
        (unsigned long)fs.f_type==0x62656572UL || (unsigned long)fs.f_type==0x1373UL) fail();
    return fd;
}
static int stage_name(const char *s) {
    if (strlen(s)!=56 || strncmp(s,".sericon-upload-",16) || strcmp(s+48,".partial")) return 0;
    for (int i=16;i<48;i++) if (!strchr(hex,s[i])) return 0;
    return 1;
}
static int stage_open(const char *path, int flags) {
    char leaf[513]; struct stat st;
    int d=parent(path,leaf);
    if (!stage_name(leaf)) fail();
    int fd=openat(d,leaf,flags|O_NOFOLLOW|O_NONBLOCK|O_CLOEXEC,0600);
    close(d);
    if (fd<0 || fstat(fd,&st) || !S_ISREG(st.st_mode) || st.st_uid!=geteuid() ||
        st.st_nlink>2 || st.st_size<0 || (uint64_t)st.st_size>LIMIT) fail();
    return fd;
}
static void upload_begin(const char *dest, const char *token, const char *length) {
    char leaf[513],stage[57]; struct stat st; struct statvfs fs;
    uint64_t size=parse_number(length);
    if (strlen(token)!=32) fail();
    for (const char *p=token;*p;p++) if (!strchr(hex,*p)) fail();
    int d=parent(dest,leaf);
    if (!fstatat(d,leaf,&st,AT_SYMLINK_NOFOLLOW) || errno!=ENOENT)
        reason("destination exists or cannot be checked; uploads never replace files");
    if (fstatvfs(d,&fs) || (fs.f_flag&ST_RDONLY)) reason("destination filesystem is not writable");
    if ((uint64_t)fs.f_bavail*fs.f_frsize<size) reason("insufficient free space for upload");
    memcpy(stage,".sericon-upload-",16); memcpy(stage+16,token,32); strcpy(stage+48,".partial");
    int fd=openat(d,stage,O_WRONLY|O_CREAT|O_EXCL|O_NOFOLLOW|O_CLOEXEC,0600);
    if (fd<0 && errno==EEXIST) fd=openat(d,stage,O_WRONLY|O_NOFOLLOW|O_NONBLOCK|O_CLOEXEC);
    if (fd<0 || fstat(fd,&st) || !S_ISREG(st.st_mode) || st.st_uid!=geteuid() || st.st_nlink!=1 || st.st_size!=0) fail();
    if (fsync(fd) || close(fd) || close(d)) fail();
    payload("ready",5);
}
static void upload_write(const char *path, const char *offset, const char *encoded, const char *crc) {
    unsigned char data[CHUNK],readback[CHUNK]; struct stat st; struct sum sum={0,0};
    size_t n=strlen(encoded);
    if (!n || n>CHUNK*2 || n%2) fail();
    n/=2;
    for (size_t i=0;i<n;i++) {
        const char *a=strchr(hex,encoded[2*i]),*b=strchr(hex,encoded[2*i+1]);
        if (!a || !b) fail();
        data[i]=(unsigned char)(((a-hex)<<4)|(b-hex));
    }
    update(&sum,data,n);
    if (finish(sum)!=crc_number(crc)) reason("upload chunk checksum mismatch; no bytes written");
    uint64_t start=parse_number(offset);
    if (start+n>LIMIT) fail();
    int fd=stage_open(path,O_RDWR);
    if (fstat(fd,&st) || start>(uint64_t)st.st_size || st.st_nlink!=1) fail();
    if (lseek(fd,(off_t)start,SEEK_SET)!=(off_t)start) fail();
    for (size_t at=0;at<n;) {
        ssize_t wrote=write(fd,data+at,n-at);
        if (wrote<0 && errno==EINTR) continue;
        if (wrote<=0) reason("upload write failed; partial file retained");
        at+=(size_t)wrote;
    }
    if (fsync(fd) || lseek(fd,(off_t)start,SEEK_SET)!=(off_t)start) fail();
    size_t got=0;
    while (got<n) { ssize_t r=read_retry(fd,readback+got,n-got); if (!r) fail(); got+=(size_t)r; }
    if (memcmp(data,readback,n) || close(fd)) fail();
    payload(readback,n);
}
static int verified_fd(int fd, uint64_t length, uint32_t crc) {
    struct stat st; struct sum s={0,0}; unsigned char buf[CHUNK]; ssize_t n;
    if (fstat(fd,&st) || !S_ISREG(st.st_mode) || st.st_size<0 || (uint64_t)st.st_size!=length) return 0;
    while ((n=read_retry(fd,buf,sizeof(buf)))>0) {
        if (s.len+(uint64_t)n>length) return 0;
        update(&s,buf,(size_t)n);
    }
    return s.len==length && finish(s)==crc;
}
static void upload_commit(const char *stage, const char *dest, const char *length, const char *crc, const char *mode) {
    char a[513],b[513]; struct stat pa,pb,st,existing;
    int da=parent(stage,a),db=parent(dest,b);
    uint64_t size=parse_number(length); uint32_t expected=crc_number(crc);
    mode_t permissions=!strcmp(mode,"700")?0700:0600;
    if (strcmp(mode,"700") && strcmp(mode,"600")) fail();
    if (!stage_name(a) || fstat(da,&pa) || fstat(db,&pb) || pa.st_dev!=pb.st_dev || pa.st_ino!=pb.st_ino) fail();
    int fd=openat(da,a,O_RDWR|O_NOFOLLOW|O_NONBLOCK|O_CLOEXEC);
    if (fd<0 && errno==ENOENT) {
        /* Retry after successful publication with a lost/corrupted response. */
        fd=openat(db,b,O_RDONLY|O_NOFOLLOW|O_NONBLOCK|O_CLOEXEC);
        if (fd<0 || !verified_fd(fd,size,expected)) fail();
    } else {
        if (fd<0 || fstat(fd,&st) || st.st_uid!=geteuid() || st.st_nlink>2 || !verified_fd(fd,size,expected))
            reason("whole upload checksum mismatch; destination not published");
        if (fchmod(fd,permissions) || fsync(fd)) fail();
        if (linkat(da,a,db,b,0)) {
            if (errno!=EEXIST || fstatat(db,b,&existing,AT_SYMLINK_NOFOLLOW) ||
                existing.st_dev!=st.st_dev || existing.st_ino!=st.st_ino)
                reason("destination exists; verified partial retained without replacement");
        }
        if (unlinkat(da,a,0)) fail();
    }
    if (close(fd)) fail();
    /* Some old filesystems do not implement directory fsync. */
    if (fsync(db) && errno!=EINVAL && errno!=EROFS) fail();
    close(da); close(db); payload("committed",9);
}
static void inspect(const char *section) {
    if (!strcmp(section,"identity")) {
        struct utsname u; uint16_t endian=1;
        if (uname(&u)) fail();
        field("ok");
        field("sysname"); field(u.sysname); field("release"); field(u.release);
        field("version"); field(u.version); field("machine"); field(u.machine);
        field("hostname"); field(u.nodename); field("helper_arch"); field(SC_ARCH);
        field("byte_order"); field(*(unsigned char *)&endian?"little":"big");
        field("helper_bits"); number_field(sizeof(void*)*8);
        field("effective_uid"); number_field(geteuid()); field("effective_gid"); number_field(getegid());
        return;
    }
    if (!strcmp(section,"storage")) {
        const char *paths[]={"/","/tmp","/var/tmp","/run","/dev/shm"};
        field("ok");
        for (unsigned i=0;i<sizeof(paths)/sizeof(paths[0]);i++) {
            struct statvfs fs;
            field(paths[i]);
            if (statvfs(paths[i],&fs)) { field("unavailable"); field(""); field(""); field(""); }
            else {
                field("ok"); number_field((uint64_t)fs.f_blocks*fs.f_frsize);
                number_field((uint64_t)fs.f_bavail*fs.f_frsize);
                field(!(fs.f_flag&ST_RDONLY) && !access(paths[i],W_OK|X_OK)?"yes":"no");
            }
        }
        return;
    }
    const char *path=NULL;
    if (!strcmp(section,"cpu")) path="/proc/cpuinfo";
    else if (!strcmp(section,"memory")) path="/proc/meminfo";
    else if (!strcmp(section,"uptime")) path="/proc/uptime";
    else if (!strcmp(section,"mounts")) path="/proc/mounts";
    else if (!strcmp(section,"flash")) path="/proc/mtd";
    else fail();
    int fd=open(path,O_RDONLY|O_NONBLOCK|O_CLOEXEC);
    if (fd<0) { field("unavailable"); return; }
    static unsigned char buf[8192]; size_t used=0; ssize_t n;
    while (used<sizeof(buf)) {
        n=read(fd,buf+used,sizeof(buf)-used);
        if (n<0 && errno==EINTR) continue;
        if (n<0) { close(fd); field("unavailable"); return; }
        if (!n) break;
        used+=(size_t)n;
    }
    unsigned char extra; do { n=read(fd,&extra,1); } while (n<0 && errno==EINTR);
    close(fd); field(n>0?"truncated":n<0?"unavailable":"ok"); payload(buf,used);
}
int main(int argc, char **argv) {
    if (argc<3 || strlen(argv[1])!=34 || strncmp(argv[1],"SC",2)) fail();
    for (const char *p=argv[1]+2; *p; p++)
        if (!strchr(hex,*p)) fail();
    if (!strcmp(argv[2],"info") && argc==3) {
        static const char info[]="sericon-helper/2 " SC_ARCH " check read list upload inspect";
        payload(info,sizeof(info)-1);
    } else if (!strcmp(argv[2],"check") && argc==4) checksum_file(argv[3]);
    else if (!strcmp(argv[2],"read") && argc==5) read_chunk(argv[3],argv[4]);
    else if (!strcmp(argv[2],"list") && argc==4) list(argv[3]);
    else if (!strcmp(argv[2],"upload-begin") && argc==6) upload_begin(argv[3],argv[4],argv[5]);
    else if (!strcmp(argv[2],"upload-write") && argc==7) upload_write(argv[3],argv[4],argv[5],argv[6]);
    else if (!strcmp(argv[2],"upload-commit") && argc==8) upload_commit(argv[3],argv[4],argv[5],argv[6],argv[7]);
    else if (!strcmp(argv[2],"inspect") && argc==4) inspect(argv[3]);
    else fail();
    text("\n"); text(argv[1]); text(":C\n");
    number(finish(output)); text(" "); number(output.len); text("\n");
    return 0;
}
