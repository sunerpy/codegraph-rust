#pragma once

struct HeaderBits {
    unsigned enabled : 1;
};

static inline int choose_value(int flag)
{
label:
    return flag ? 1 : 0;
}

static const char *header_text = "class HeaderGhost : HeaderBits";

/*
struct HeaderCommentGhost : HeaderBits {
};
*/
