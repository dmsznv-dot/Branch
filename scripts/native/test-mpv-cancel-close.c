// Test-only barriers around the real mpv failed-open/shutdown path. The cookie
// remains alive through join: a wrong order is reported without accessing freed
// memory or relying on a crash. No production objects/library are overwritten.
#include <pthread.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <mpv/client.h>
#include <mpv/stream_cb.h>

#define CHECK(condition) do { if (!(condition)) { \
    fputs("native_order_test_setup_failed\n", stderr); _Exit(2); \
} } while (0)

struct mp_cancel;
void mp_cancel_trigger(struct mp_cancel *cancel);
static pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t wake = PTHREAD_COND_INITIALIZER;
static bool close_entered, trigger_done, release_close;
static unsigned close_calls, late_cancels, read_calls;
static FILE *optional_file;
static int cookie; // intentionally alive until native shutdown has joined

// The runner substitutes only the real open/playback cancellation calls with this
// forwarding observer. Completion proves shutdown traversed the real parent
// cancellation tree while close_fn was paused; no timing-based inference.
void branch_test_trigger(struct mp_cancel *cancel)
{
    mp_cancel_trigger(cancel);
    pthread_mutex_lock(&lock);
    if (close_entered) {
        trigger_done = true;
        pthread_cond_broadcast(&wake);
    }
    pthread_mutex_unlock(&lock);
}

static void wait_flag(bool *flag)
{
    struct timespec deadline;
    CHECK(clock_gettime(CLOCK_REALTIME, &deadline) == 0);
    deadline.tv_sec += 5;
    while (!*flag) {
        if (pthread_cond_timedwait(&wake, &lock, &deadline) != 0) {
            fprintf(stderr, "barrier_timeout close=%d trigger=%d reads=%u\n",
                    close_entered, trigger_done, read_calls);
            _Exit(2);
        }
    }
}

static int64_t read_invalid(void *data, char *buffer, uint64_t count)
{
    CHECK(data == &cookie);
    (void)buffer;
    (void)count;
    pthread_mutex_lock(&lock);
    read_calls++;
    pthread_mutex_unlock(&lock);
    if (optional_file) {
        size_t n = fread(buffer, 1, (size_t)count, optional_file);
        return ferror(optional_file) ? -1 : (int64_t)n;
    }
    return 0; // invalid/empty media forces actual demux-open failure cleanup
}

static void cancel_cookie(void *data)
{
    CHECK(data == &cookie);
    pthread_mutex_lock(&lock);
    if (close_entered)
        late_cancels++;
    pthread_mutex_unlock(&lock);
}

static void close_cookie(void *data)
{
    CHECK(data == &cookie);
    pthread_mutex_lock(&lock);
    close_calls++;
    close_entered = true;
    pthread_cond_broadcast(&wake);
    if (!optional_file)
        wait_flag(&release_close);
    pthread_mutex_unlock(&lock);
}

static int64_t seek_file(void *data, int64_t offset)
{
    CHECK(data == &cookie && optional_file);
    if (offset < 0 || fseeko(optional_file, offset, SEEK_SET) != 0)
        return -1;
    return offset;
}

static int64_t size_file(void *data)
{
    CHECK(data == &cookie && optional_file);
    off_t current = ftello(optional_file);
    if (current < 0 || fseeko(optional_file, 0, SEEK_END) != 0)
        return -1;
    off_t size = ftello(optional_file);
    CHECK(fseeko(optional_file, current, SEEK_SET) == 0);
    return size;
}

static int open_cookie(void *data, char *uri, mpv_stream_cb_info *info)
{
    CHECK(data == &cookie);
    (void)uri;
    *info = (mpv_stream_cb_info){.cookie = data, .read_fn = read_invalid,
        .close_fn = close_cookie, .cancel_fn = optional_file ? NULL : cancel_cookie,
        .seek_fn = optional_file ? seek_file : NULL,
        .size_fn = optional_file ? size_file : NULL};
    return 0;
}

static void *shutdown_core(void *handle)
{
    mpv_terminate_destroy(handle);
    return NULL;
}

int main(int argc, char **argv)
{
    CHECK(argc == 1 || (argc == 3 && strcmp(argv[1], "--without-cancel") == 0));
    if (argc == 3) {
        optional_file = fopen(argv[2], "rb");
        CHECK(optional_file);
    }
    mpv_handle *handle = mpv_create();
    CHECK(handle);
    const char *options[][2] = {
        {"config", "no"}, {"load-scripts", "no"}, {"ytdl", "no"},
        {"terminal", "no"}, {"msg-level", "all=no"}, {"vo", "null"},
        {"ao", "null"}, {"demuxer", "lavf"}, {"access-references", "no"},
        {"demuxer-lavf-o", "format_whitelist=%12%mov,matroska"},
    };
    for (unsigned i = 0; i < sizeof(options) / sizeof(options[0]); i++)
        CHECK(mpv_set_option_string(handle, options[i][0], options[i][1]) == 0);
    CHECK(mpv_stream_cb_add_ro(handle, "ordertest", &cookie, open_cookie) == 0);
    CHECK(mpv_initialize(handle) == 0);
    const char *command[] = {"loadfile", "ordertest://invalid", NULL};
    CHECK(mpv_command_async(handle, 1, command) == 0);
    if (optional_file) {
        struct timespec start, now;
        CHECK(clock_gettime(CLOCK_MONOTONIC, &start) == 0);
        for (;;) {
            mpv_event *event = mpv_wait_event(handle, 0.1);
            if (event->event_id == MPV_EVENT_FILE_LOADED)
                break;
            CHECK(event->event_id != MPV_EVENT_END_FILE);
            CHECK(clock_gettime(CLOCK_MONOTONIC, &now) == 0);
            CHECK(now.tv_sec - start.tv_sec < 5);
        }
        mpv_terminate_destroy(handle);
        CHECK(close_calls == 1 && read_calls > 0 && late_cancels == 0);
        CHECK(fclose(optional_file) == 0);
        puts("optional_cancel_stream_close_verified loaded=1 close_calls=1 cancel_fn=NULL");
        return 0;
    }
    pthread_mutex_lock(&lock);
    wait_flag(&close_entered);
    pthread_mutex_unlock(&lock);
    pthread_t shutdown;
    CHECK(pthread_create(&shutdown, NULL, shutdown_core, handle) == 0);
    pthread_mutex_lock(&lock);
    wait_flag(&trigger_done);
    unsigned observed_late = late_cancels;
    release_close = true;
    pthread_cond_broadcast(&wake);
    pthread_mutex_unlock(&lock);
    CHECK(pthread_join(shutdown, NULL) == 0);
    CHECK(close_calls == 1 && read_calls > 0);
    printf("failed_open_shutdown close_calls=%u late_cancels=%u trigger_completed=1\n",
           close_calls, observed_late);
    if (observed_late) {
        fprintf(stderr, "cancel_after_close_entry\n");
        return 1;
    }
    puts("native_cancel_close_order_verified");
    return 0;
}
