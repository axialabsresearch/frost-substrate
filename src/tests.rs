#![allow(unused_imports)]
#![allow(unused_variables)]

use crate::mock::{new_test_ext, Test};
use frame_support::assert_ok;

#[test]
fn it_works() {
    new_test_ext().execute_with(|| {
        // Your test code here
        assert_eq!(2 + 2, 4);
    });
} 