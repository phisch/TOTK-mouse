#include "lib.hpp"
#include "loggers.hpp"
#include <cmath>

/*
 * totk-mouse: mouse-driven camera for TOTK 1.4.1 (main build 965EAB9C...).
 *
 * The gameplay camera calculator at main+0x47e3a8 fills a struct
 * {eye f32x3, target f32x3, up f32x3, fov} on its stack by calling main+0x4805cc,
 * then runs collision and pull-in on it before the result is copied down to the
 * committed sead::LookAtCamera. Right after that call (main+0x47e664) x26 points at
 * the struct. We replace the eye's direction there with our own yaw/pitch at the
 * game's distance, so everything after it, collision included, works on our view.
 *
 * Mouse input arrives from the host companion (companion/) that writes running totals
 * into `g_shared`, found by its signature in guest RAM. The companion grabs the
 * mouse, so the emulator never sees its buttons. It sends the mapped Switch buttons
 * instead, and we OR them into every controller state the game reads.
 */

namespace nn::hid {
struct NpadFullKeyState;
struct NpadHandheldState;
struct NpadJoyDualState;
struct NpadJoyLeftState;
struct NpadJoyRightState;
int GetNpadStates(NpadFullKeyState* out, int count, const unsigned int& id);
int GetNpadStates(NpadHandheldState* out, int count, const unsigned int& id);
int GetNpadStates(NpadJoyDualState* out, int count, const unsigned int& id);
int GetNpadStates(NpadJoyLeftState* out, int count, const unsigned int& id);
int GetNpadStates(NpadJoyRightState* out, int count, const unsigned int& id);
}  // namespace nn::hid

namespace {

/* exlaunch's float inline trampoline pushes x0..x29 then q0..q31 below them and
   passes sp, so the real layout is [q0..q31][x0..x29], the reverse of what
   InlineFloatCtx declares. Index the real layout directly. */
inline uint64_t RegX(exl::hook::InlineFloatCtx* ctx, int n) {
    return *reinterpret_cast<uint64_t*>(reinterpret_cast<uintptr_t>(ctx) + 0x200 + n * 8);
}

constexpr uintptr_t CameraCalcOffset = 0x47e664;
/* The instruction we expect at the hook site: `ldr x8, [x19, #1520]`. Every offset
   here is specific to game version 1.4.1. On any other version this address holds
   different code, and patching it would crash the game, so refuse. */
constexpr uint32_t CameraCalcExpected = 0xf942fa68;

constexpr double WorldScale = 50.0;
constexpr double PitchLimit = 1.40;     /* radians from horizontal, ~80 degrees */
constexpr double TwoPi = 6.283185307179586;

/* Shared with the companion. The signature must be unique in guest RAM, so it is
   only ever assembled at runtime, never stored whole in the binary. */
struct SharedBlock {
    char magic[8];              /* "TMOUSE01" */
    uint32_t version;
    uint32_t cookie;
    volatile int64_t total_x;   /* running mouse counts, written by the companion */
    volatile int64_t total_y;
    volatile uint32_t seq;      /* bumped by the companion on every write */
    volatile uint32_t enabled;  /* 0 = leave the camera alone */
    volatile float sens_x;      /* radians per mouse count; negative inverts */
    volatile uint32_t heartbeat;/* bumped by us every gameplay camera update */
    volatile uint32_t applied;  /* bumped by us whenever we rotated the camera */
    volatile uint32_t pad_buttons; /* nn::hid::NpadButton bits to hold down */
    uint32_t reserved;
    volatile float sens_y;      /* radians per mouse count; negative inverts */
};
static_assert(sizeof(SharedBlock) == 64);

constexpr uint32_t Version = 6;
constexpr uint32_t Cookie = 0x5a17c3e9;

alignas(64) SharedBlock g_shared;

int64_t g_last_x = 0, g_last_y = 0;
bool g_have_last = false;

/* Our absolute view direction around the target. Taken from the game's view
   whenever mouselook turns on, then driven only by the mouse. */
bool g_active = false;
double g_yaw = 0.0, g_pitch = 0.0;

void InitShared() {
    const char m[8] = {'T', 'M', 'O', 'U', 'S', 'E', '0', '1'};
    for (int i = 0; i < 8; ++i) g_shared.magic[i] = m[i];
    g_shared.version = Version;
    g_shared.cookie = Cookie;
    g_shared.sens_x = 0.0025f;
    g_shared.sens_y = 0.0025f;
    g_shared.enabled = 0;
    g_shared.pad_buttons = 0;
}

/* Every Npad*State starts with the same layout. */
struct NpadBaseState {
    int64_t sampling_number;
    uint64_t buttons;
    int32_t stick_l[2];
    int32_t stick_r[2];
    uint32_t attributes;
    uint32_t reserved;
};

constexpr unsigned int NpadIdNo1 = 0;
constexpr unsigned int NpadIdHandheld = 0x20;

void AddPadButtons(void* states, int n, unsigned int id) {
    const uint32_t extra = g_shared.pad_buttons;
    if (extra == 0 || !g_shared.enabled || (id != NpadIdNo1 && id != NpadIdHandheld)) {
        return;
    }
    auto* s = static_cast<NpadBaseState*>(states);
    for (int i = 0; i < n; ++i) {
        s[i].buttons |= extra;
    }
}

template <typename State>
struct NpadHook : public exl::hook::impl::TrampolineHook<NpadHook<State>> {
    static int Callback(State* out, int count, const unsigned int& id) {
        const int n = NpadHook::Orig(out, count, id);
        AddPadButtons(out, n, id);
        return n;
    }
    static void Install() {
        int (*fn)(State*, int, const unsigned int&) = &nn::hid::GetNpadStates;
        NpadHook::InstallAtFuncPtr(fn);
    }
};

}  // namespace

HOOK_DEFINE_INLINE(CameraCalcHook) {
    static void Callback(exl::hook::InlineFloatCtx* ctx) {
        float* cam = reinterpret_cast<float*>(RegX(ctx, 26));
        const double ex = cam[0], ey = cam[1], ez = cam[2];
        const double tx = cam[3], ty = cam[4], tz = cam[5];
        const double am = fabs(tx) > fabs(tz) ? fabs(tx) : fabs(tz);
        if (am < WorldScale && fabs(ty) < WorldScale) {
            return;     /* menu/preview camera */
        }
        g_shared.heartbeat = g_shared.heartbeat + 1;

        /* Totals rather than deltas, so a missed frame loses nothing. */
        const int64_t x = g_shared.total_x, y = g_shared.total_y;
        if (!g_have_last) {
            g_last_x = x;
            g_last_y = y;
            g_have_last = true;
        }
        const int64_t dx = x - g_last_x, dy = y - g_last_y;
        g_last_x = x;
        g_last_y = y;
        if (!g_shared.enabled) {
            g_active = false;
            return;
        }

        const double vx = ex - tx, vy = ey - ty, vz = ez - tz;
        const double h = sqrt(vx * vx + vz * vz);
        const double dist = sqrt(h * h + vy * vy);
        if (dist < 0.01) {
            return;
        }
        if (!g_active) {
            g_active = true;
            g_yaw = atan2(vx, vz);
            g_pitch = atan2(vy, h);
        }

        g_yaw -= dx * g_shared.sens_x;
        if (g_yaw > TwoPi) g_yaw -= TwoPi;
        if (g_yaw < -TwoPi) g_yaw += TwoPi;
        g_pitch += dy * g_shared.sens_y;
        if (g_pitch > PitchLimit) g_pitch = PitchLimit;
        if (g_pitch < -PitchLimit) g_pitch = -PitchLimit;

        const double ch = cos(g_pitch) * dist;
        cam[0] = static_cast<float>(tx + ch * sin(g_yaw));
        cam[1] = static_cast<float>(ty + sin(g_pitch) * dist);
        cam[2] = static_cast<float>(tz + ch * cos(g_yaw));
        g_shared.applied = g_shared.applied + 1;
    }
};

extern "C" void exl_main(void*, void*) {
    exl::hook::Initialize();
    const uintptr_t site = exl::util::modules::GetTargetStart() + CameraCalcOffset;
    const uint32_t found = *reinterpret_cast<volatile uint32_t*>(site);
    if (found != CameraCalcExpected) {
        /* No shared block either, so the companion simply never finds us. */
        Logging.Log("totk-mouse: unsupported game version (found 0x%08x at main+0x%lx, "
                    "expected 0x%08x); not hooking", found, (unsigned long)CameraCalcOffset,
                    CameraCalcExpected);
        return;
    }
    InitShared();
    CameraCalcHook::InstallAtOffset(CameraCalcOffset);
    NpadHook<nn::hid::NpadFullKeyState>::Install();
    NpadHook<nn::hid::NpadHandheldState>::Install();
    NpadHook<nn::hid::NpadJoyDualState>::Install();
    NpadHook<nn::hid::NpadJoyLeftState>::Install();
    NpadHook<nn::hid::NpadJoyRightState>::Install();
    Logging.Log("totk-mouse: camera hook at main+0x%lx, shared block at %p",
                (unsigned long)CameraCalcOffset, &g_shared);
}

extern "C" NORETURN void exl_exception_entry() {
    EXL_ABORT("totk-mouse exception");
}
