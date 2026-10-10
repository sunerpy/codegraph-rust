<?php

namespace App\Field;

class Registry extends Base
{
    public static function create(): static
    {
        return new static();
    }

    public static function build(): static
    {
        return static::create();
    }

    public function labelText(): string
    {
        return parent::label();
    }
}
