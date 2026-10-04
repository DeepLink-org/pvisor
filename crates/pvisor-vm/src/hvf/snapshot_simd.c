#include <Hypervisor/Hypervisor.h>
#include <string.h>

// HV SIMD values use the vector ABI, not the integer-u128 ABI emitted by
// bindgen. Keep both directions behind a byte-buffer ABI Rust can express.
hv_return_t krun_snapshot_get_q(hv_vcpu_t cpu, uint32_t reg, unsigned char *out) {
    hv_simd_fp_uchar16_t value;
    hv_return_t ret = hv_vcpu_get_simd_fp_reg(cpu, reg, &value);
    if (ret == HV_SUCCESS) memcpy(out, &value, 16);
    return ret;
}
hv_return_t krun_snapshot_set_q(hv_vcpu_t cpu, uint32_t reg, const unsigned char *in) {
    hv_simd_fp_uchar16_t value;
    memcpy(&value, in, 16);
    return hv_vcpu_set_simd_fp_reg(cpu, reg, value);
}
