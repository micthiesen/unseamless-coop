// uinput-tap.c — a tiny, self-contained virtual-keyboard key tapper for the remote rig's `dismiss`.
//
// Why this instead of ydotool: all `deck.sh dismiss` needs is to tap one key (Enter) N times to clear
// ELDEN RING's modal startup popups and select "Continue" into gameplay. ydotool would drag in a daemon
// + a unix socket + a client (awkward to keep alive over SSH); this does the whole job with no daemon,
// no socket, and one statically-linked binary. It writes directly to /dev/uinput, which on the Deck is
// already accessible to the `deck` user via the SteamOS session ACL (60-cecd-uinput.rules), so it needs
// no root. Built natively-static on the dev box (x86_64 -> x86_64) and rsync'd to the Deck by deck.sh.
//
// Usage:  uinput-tap [keycode] [count] [interval_ms]
//   keycode      Linux input keycode (default 28 = KEY_ENTER). See <linux/input-event-codes.h>.
//   count        number of down+up taps (default 30). count=0 creates+destroys the device with NO taps,
//                which is the safe "does the uinput path work?" smoke test (no input is injected).
//   interval_ms  delay between taps in ms (default 400).
//
// Re-derivation: this is just the kernel uinput "create a virtual device and emit events" recipe
// (Documentation/input/uinput.rst). If the Deck's kernel ever changes the uinput_setup ABI, that doc is
// the reference.

#include <linux/uinput.h>
#include <sys/ioctl.h>
#include <fcntl.h>
#include <unistd.h>
#include <string.h>
#include <stdlib.h>
#include <stdio.h>
#include <errno.h>
#include <time.h>

static int emit(int fd, int type, int code, int val) {
    struct input_event ie;
    memset(&ie, 0, sizeof(ie));
    ie.type = (unsigned short)type;
    ie.code = (unsigned short)code;
    ie.value = val;
    return write(fd, &ie, sizeof(ie)) == (ssize_t)sizeof(ie) ? 0 : -1;
}

static void msleep(long ms) {
    struct timespec ts = { ms / 1000, (ms % 1000) * 1000000L };
    nanosleep(&ts, NULL);
}

int main(int argc, char **argv) {
    int key         = argc > 1 ? atoi(argv[1]) : KEY_ENTER;   // 28
    int count       = argc > 2 ? atoi(argv[2]) : 30;
    long interval   = argc > 3 ? atol(argv[3]) : 400;
    if (count < 0) count = 0;
    if (interval < 0) interval = 0;

    int fd = open("/dev/uinput", O_WRONLY | O_NONBLOCK);
    if (fd < 0) {
        fprintf(stderr, "uinput-tap: open /dev/uinput: %s\n", strerror(errno));
        return 1;
    }

    if (ioctl(fd, UI_SET_EVBIT, EV_KEY) < 0 || ioctl(fd, UI_SET_KEYBIT, key) < 0) {
        fprintf(stderr, "uinput-tap: UI_SET_*BIT: %s\n", strerror(errno));
        close(fd);
        return 1;
    }

    struct uinput_setup usetup;
    memset(&usetup, 0, sizeof(usetup));
    usetup.id.bustype = BUS_USB;
    usetup.id.vendor  = 0x1209;   // pid.codes generic
    usetup.id.product = 0x5543;
    strncpy(usetup.name, "unseamless-deck-tap", sizeof(usetup.name) - 1);
    if (ioctl(fd, UI_DEV_SETUP, &usetup) < 0 || ioctl(fd, UI_DEV_CREATE) < 0) {
        fprintf(stderr, "uinput-tap: UI_DEV_CREATE: %s\n", strerror(errno));
        close(fd);
        return 1;
    }

    // Let the compositor enumerate the new device before we send anything, else early taps are dropped.
    msleep(500);

    int rc = 0;
    for (int i = 0; i < count; i++) {
        if (emit(fd, EV_KEY, key, 1) || emit(fd, EV_SYN, SYN_REPORT, 0) ||
            emit(fd, EV_KEY, key, 0) || emit(fd, EV_SYN, SYN_REPORT, 0)) {
            fprintf(stderr, "uinput-tap: write event: %s\n", strerror(errno));
            rc = 1;
            break;
        }
        if (i + 1 < count) msleep(interval);
    }

    ioctl(fd, UI_DEV_DESTROY);
    close(fd);
    return rc;
}
