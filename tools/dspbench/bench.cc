// Benchmark wrapper: Braids' macro oscillator (MIT, Emilie Gillet) rendered
// in its 24-sample blocks, for counting ColdFire instructions per sample.
#include <new>
#include "braids/macro_oscillator.h"

using namespace braids;

alignas(4) static unsigned char storage[sizeof(MacroOscillator)];
static MacroOscillator* osc;
static uint8_t sync_buf[24];
int16_t out[24 * 64];

extern "C" void bench_init(int shape) {
  osc = new (storage) MacroOscillator;
  osc->Init();
  osc->set_shape(static_cast<MacroOscillatorShape>(shape));
  osc->set_pitch(60 << 7);           // middle C, Braids' 7-bit fractional semitones
  osc->set_parameters(16384, 16384);
  osc->Strike();
}

extern "C" void bench_render(int blocks) {
  for (int b = 0; b < blocks; ++b) {
    osc->Render(sync_buf, out + 24 * (b & 63), 24);
  }
}
