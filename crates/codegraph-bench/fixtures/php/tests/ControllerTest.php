<?php

namespace Tests;

use App\Controller;

class ControllerTest extends TestCase
{
    public function testShow(): void
    {
        $controller = new Controller();
        $this->assertEquals(3, count($controller->show()));
    }
}
