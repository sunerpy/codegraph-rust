<?php

function trans(string $key): string
{
    return $key;
}

function formatPrice(int $cents): string
{
    return number_format($cents / 100, 2);
}
