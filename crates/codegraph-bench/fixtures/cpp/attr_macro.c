#define SEC_ATTR __attribute__((section(".init")))
#define VOID void

typedef unsigned int UINT32;

SEC_ATTR VOID GoodName (VOID) {
}

SEC_ATTR UINT32 LostName (VOID) {
    return 0;
}

UINT32 NoAttr (void) {
    return 0;
}

SEC_ATTR UINT32 *PtrRet (VOID) {
    return 0;
}
