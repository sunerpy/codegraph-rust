<?php

namespace App\Field;

interface Renders
{
    public function render(): string;
}

class FirstName extends Base implements Renders
{
    public static function make(): self
    {
        return new self();
    }

    public function render(): string
    {
        return $this->label() . self::suffix();
    }

    private static function suffix(): string
    {
        return '!';
    }
}
