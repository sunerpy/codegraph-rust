void reset_profile(void)
{
    RESET_CONFIG(profile_t, profile,
        .pid = { [0] = 10, [1] = { 50, 75 } },
        .limit = 500,
    );
}

void after_reset(void)
{
}

int final_value(void)
{
    return 1;
}
