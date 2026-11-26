<?php
namespace App\Http;

use App\Events\Registered;

class Registration
{
    public function register(): void
    {
        event(new Registered());
    }
}
