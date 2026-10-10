<?php

namespace App\Field;

abstract class Base
{
    protected function label(): string
    {
        return static::class;
    }
}
